//! The core thread: owns the session, the editor and the renderer, turns
//! window events and pty output into state, and draws frames on demand.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::TermMode;

use crate::complete::{self, Candidate, Commands, Completion, Kind};
use crate::editor::{Editor, History};
use crate::gpu::Gpu;
use crate::keys::{self, Named};
use crate::platform::{self, Cursor, KeyEvent, UiEvent};
use crate::scene::{Quad, Rect, Scene, LAYER_CONTENT, LAYER_OVERLAY};
use crate::session::{BlockKind, Credits, GridSize, Notice, Phase, Session};
use crate::text::{Style, TextSystem, UI, UI_BOLD};
use crate::theme::{Color, Theme};
use crate::ui::{self, GridView, Span};
use crate::Msg;

const PAD_X: f32 = 14.0;
const ALT_PAD_Y: f32 = 8.0;
const BLOCK_PAD_TOP: f32 = 10.0;
const META_HEIGHT: f32 = 18.0;
const OUTPUT_GAP: f32 = 4.0;
const BLOCK_PAD_BOTTOM: f32 = 12.0;
const CHIP_HEIGHT: f32 = 22.0;
const INPUT_PAD: f32 = 10.0;
const DEFAULT_FONT_SIZE: f32 = 14.0;
const BLINK: Duration = Duration::from_millis(530);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const MAX_EDITOR_LINES: usize = 10;
const MENU_ROWS: usize = 8;
/// The launch banner, in figlet's "ANSI Shadow" style.
const BANNER: [&str; 6] = [
    "████████╗███████╗███╗   ███╗██╗███╗   ██╗ ██████╗ ",
    "╚══██╔══╝██╔════╝████╗ ████║██║████╗  ██║██╔═══██╗",
    "   ██║   █████╗  ██╔████╔██║██║██╔██╗ ██║██║   ██║",
    "   ██║   ██╔══╝  ██║╚██╔╝██║██║██║╚██╗██║██║   ██║",
    "   ██║   ███████╗██║ ╚═╝ ██║██║██║ ╚████║╚██████╔╝",
    "   ╚═╝   ╚══════╝╚═╝     ╚═╝╚═╝╚═╝  ╚═══╝ ╚═════╝ ",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectMode {
    Char,
    Word,
    Line,
}

/// A mouse selection inside one block's output, in (line, column).
#[derive(Clone, Copy)]
struct Selection {
    block: u64,
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
}

pub struct App {
    gpu: Gpu,
    text: TextSystem,
    theme: Theme,
    scene: Scene,
    session: Session,
    editor: Editor,
    history: History,
    commands: Commands,
    completion: Option<Completion>,
    sender: Sender<Msg>,
    credits: Arc<Credits>,
    width: u32,
    height: u32,
    /// Distance of the viewport's bottom above the content's bottom.
    scroll: f32,
    scroll_target: f32,
    selected: Option<u64>,
    selection: Option<Selection>,
    dragging: bool,
    focused: bool,
    blink_epoch: Instant,
    layout: Layout,
    toast: Option<(String, Instant)>,
    bell: Option<Instant>,
    dirty: bool,
    reflow_at: Option<Instant>,
    title: String,
    cursor: Cursor,
    quitting: bool,
    user_host: String,
    /// Content height of the previous frame, to keep a scrolled-up view
    /// still while output grows below it.
    content_height: f32,
    /// When the frame on screen goes stale on its own (spinner, blink).
    animation_at: Option<Instant>,
}

pub fn run(receiver: Receiver<Msg>, sender: Sender<Msg>, width: u32, height: u32) {
    let mut app = match App::new(sender, width, height) {
        Ok(app) => app,
        Err(error) => {
            crate::fatal(&error);
            platform::quit();
            return;
        }
    };
    crate::trace("app: event loop");
    loop {
        let message = match app.next_wakeup() {
            Some(at) => receiver.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(message) => {
                crate::trace(match &message {
                    Msg::Ui(_) => "loop: ui event",
                    Msg::Pty(_) => "loop: pty data",
                    Msg::ShellExited(_) => "loop: shell exited",
                });
                app.handle(message)
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Coalesce everything already queued into one frame, within reason.
        let budget = Instant::now() + Duration::from_millis(12);
        while Instant::now() < budget {
            match receiver.try_recv() {
                Ok(message) => app.handle(message),
                Err(_) => break,
            }
        }
        app.tick();
        if app.quitting {
            break;
        }
        if app.dirty {
            app.render();
        }
    }
}

/// Renders one frame without a window and writes it as a PPM: checks the
/// Vulkan path on its own. Returns the process exit code.
pub fn selftest(path: &str, width: u32, height: u32, frames: usize) -> i32 {
    let started = Instant::now();
    let mut gpu = match Gpu::new() {
        Ok(gpu) => gpu,
        Err(error) => {
            eprintln!("selftest: {error}");
            return 1;
        }
    };
    println!("device: {} ({:.1} ms)", gpu.device_name, started.elapsed().as_secs_f64() * 1000.0);
    let mut text = TextSystem::new(DEFAULT_FONT_SIZE);
    let theme = Theme::dark();
    let mut scene = Scene::new();
    let mut slowest = 0.0f64;
    for round in 0..frames {
        let frame = Instant::now();
        scene.reset(width as f32, height as f32, theme.background);
        let card = Rect::new(16.0, 16.0, 448.0, 128.0);
        scene.shadow(card, Color::hex(0x000000).alpha(0.6), 10.0, 8.0);
        scene.quad(Quad::new(card, theme.surface).radius(10.0).border(1.0, theme.divider));
        text.draw_text(&mut scene, "Temino self-test", 32.0, 52.0, UI_BOLD, 20.0, theme.text);
        text.draw_mono(&mut scene, "ls -la ~/config ─┼─ ✓", 32.0, 70.0, Style::default(), theme.success);
        scene.fill(Rect::new(32.0, 110.0, 120.0, 18.0), theme.accent);
        match gpu.render(&scene, &mut text.atlas) {
            Ok(pixels) => {
                let ms = frame.elapsed().as_secs_f64() * 1000.0;
                slowest = if round > 0 { slowest.max(ms) } else { slowest };
                if round < 3 || round % 100 == 0 {
                    println!("frame {round}: {ms:.2} ms");
                }
                if round + 1 == frames {
                    println!("{frames} frames, slowest after the first {slowest:.2} ms");
                    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                    for bgra in pixels.chunks_exact(4) {
                        ppm.extend_from_slice(&[bgra[2], bgra[1], bgra[0]]);
                    }
                    if let Err(error) = std::fs::write(path, ppm) {
                        eprintln!("selftest: writing {path}: {error}");
                        return 1;
                    }
                    println!("wrote {path}");
                }
            }
            Err(error) => {
                eprintln!("selftest: {error}");
                return 1;
            }
        }
    }
    0
}

fn user_host() -> String {
    let user = std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(platform::user_name)
        .unwrap_or_else(|| "user".into());
    let host = std::fs::read_to_string("/boot/system/settings/network/hostname")
        .ok()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "haiku".into());
    format!("{user}@{host}")
}

impl App {
    fn new(sender: Sender<Msg>, width: u32, height: u32) -> Result<App, String> {
        let gpu = Gpu::new()?;
        let text = TextSystem::new(DEFAULT_FONT_SIZE);
        crate::trace("app: fonts ready");
        let theme = Theme::dark();
        platform::set_background(theme.background.to_rgb8());
        let size = grid_size(width, height, &text);
        let mut session = Session::new(size);
        session.set_cell_size(text.cell.width, text.cell.height);
        let credits = Credits::new();
        session.start(sender.clone(), credits.clone()).map_err(|e| format!("starting the shell: {e}"))?;
        crate::trace("app: shell started");
        let mut app = App {
            gpu,
            text,
            theme,
            scene: Scene::new(),
            session,
            editor: Editor::new(),
            history: History::load(),
            commands: Commands::new(),
            completion: None,
            sender,
            credits,
            width,
            height,
            scroll: 0.0,
            scroll_target: 0.0,
            selected: None,
            selection: None,
            dragging: false,
            focused: true,
            blink_epoch: Instant::now(),
            layout: Layout::default(),
            toast: None,
            bell: None,
            dirty: true,
            reflow_at: None,
            title: String::new(),
            cursor: Cursor::IBeam,
            quitting: false,
            user_host: user_host(),
            content_height: 0.0,
            animation_at: None,
        };
        app.update_title();
        Ok(app)
    }

    fn handle(&mut self, message: Msg) {
        match message {
            Msg::Pty(bytes) => {
                let notices = self.session.feed(&bytes, &self.theme);
                self.credits.release();
                for notice in notices {
                    self.notice(notice);
                }
                self.dirty = true;
            }
            Msg::ShellExited(code) => {
                self.session.shell_exited(code);
                self.dirty = true;
            }
            Msg::Ui(event) => self.ui_event(event),
        }
        self.update_title();
    }

    fn notice(&mut self, notice: Notice) {
        match notice {
            Notice::Title(title) => {
                if let Some(block) = self.session.active_mut() {
                    block.command = if block.command.is_empty() { title } else { block.command.clone() };
                }
            }
            Notice::Clipboard(text) => {
                platform::clipboard_set(&text);
                self.show_toast("Copied to the clipboard by the program");
            }
            Notice::Bell => self.bell = Some(Instant::now()),
            Notice::Clear => {
                self.session.clear();
                self.selection = None;
                self.selected = None;
                self.scroll = 0.0;
                self.scroll_target = 0.0;
            }
            Notice::Finished => {}
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.session.tick(now) {
            self.dirty = true;
        }
        if self.reflow_at.is_some_and(|at| now >= at) {
            self.reflow_at = None;
            self.session.reflow();
            self.dirty = true;
        }
        if self.toast.as_ref().is_some_and(|(_, at)| now >= *at) {
            self.toast = None;
            self.dirty = true;
        }
        if self.bell.is_some_and(|at| now.duration_since(at) > Duration::from_millis(180)) {
            self.bell = None;
            self.dirty = true;
        }
        if (self.scroll - self.scroll_target).abs() > 0.5 {
            self.scroll += (self.scroll_target - self.scroll) * 0.35;
            self.dirty = true;
        } else if self.scroll != self.scroll_target {
            self.scroll = self.scroll_target;
            self.dirty = true;
        }
        // Animations that tick on their own (spinner, caret blink).
        if self.animation_at.is_some_and(|at| now >= at) {
            self.dirty = true;
        }
    }

    fn animation_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let mut want = |at: Instant| next = Some(next.map_or(at, |n| n.min(at)));
        if self.focused && (self.session.editor_active() || self.session.active().is_some()) {
            let elapsed = now.duration_since(self.blink_epoch).as_millis() as u64;
            let period = BLINK.as_millis() as u64;
            want(self.blink_epoch + Duration::from_millis((elapsed / period + 1) * period));
        }
        if self.session.active().is_some_and(|b| !b.alt_screen()) || self.session.phase == Phase::Boot {
            want(now + Duration::from_millis(80));
        }
        next
    }

    fn next_wakeup(&self) -> Option<Instant> {
        let mut times = vec![self.animation_at, self.session.deadline(), self.reflow_at];
        times.push(self.toast.as_ref().map(|(_, at)| *at));
        times.push(self.bell.map(|at| at + Duration::from_millis(200)));
        if (self.scroll - self.scroll_target).abs() > 0.0 {
            times.push(Some(Instant::now() + Duration::from_millis(16)));
        }
        times.into_iter().flatten().min()
    }

    fn update_title(&mut self) {
        let title = match self.session.active() {
            Some(block) if !block.command.is_empty() => {
                format!("{} — Temino", block.command.lines().next().unwrap_or(""))
            }
            _ => format!("{} — Temino", ui::short_path(&self.session.cwd)),
        };
        if title != self.title {
            platform::set_title(&title);
            self.title = title;
        }
    }

    fn show_toast(&mut self, message: &str) {
        self.toast = Some((message.to_owned(), Instant::now() + Duration::from_millis(1600)));
        self.dirty = true;
    }

    fn set_cursor(&mut self, cursor: Cursor) {
        if cursor != self.cursor {
            platform::set_cursor(cursor);
            self.cursor = cursor;
        }
    }

    fn pin_to_bottom(&mut self) {
        self.scroll_target = 0.0;
    }

    fn max_scroll(&self) -> f32 {
        (self.layout.content_height - self.layout.list.h).max(0.0)
    }

    fn scroll_by(&mut self, delta: f32) {
        self.scroll_target = (self.scroll_target + delta).clamp(0.0, self.max_scroll());
        self.dirty = true;
    }

    // ---- events -------------------------------------------------------

    fn ui_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Key(key) => {
                self.blink_epoch = Instant::now();
                self.key(key);
                self.dirty = true;
            }
            UiEvent::Resize { width, height } => {
                self.width = width;
                self.height = height;
                let size = grid_size(width, height, &self.text);
                self.session.resize(size);
                self.reflow_at = Some(Instant::now() + Duration::from_millis(90));
                self.dirty = true;
            }
            UiEvent::Focus(focused) => {
                self.focused = focused;
                self.blink_epoch = Instant::now();
                if let Some(block) = self.session.active() {
                    if block.term.mode().contains(TermMode::FOCUS_IN_OUT) {
                        self.session.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
                    }
                }
                self.dirty = true;
            }
            UiEvent::Close => self.close(),
            UiEvent::Wheel { dy, x, y, .. } => self.wheel(dy, x, y),
            UiEvent::MouseDown { x, y, buttons, clicks, mods } => self.mouse_down(x, y, buttons, clicks, mods),
            UiEvent::MouseMoved { x, y, buttons } => self.mouse_moved(x, y, buttons),
            UiEvent::MouseUp { x, y } => self.mouse_up(x, y),
        }
    }

    fn close(&mut self) {
        self.session.hangup();
        self.quitting = true;
        platform::quit();
    }

    fn change_font_size(&mut self, size: f32) {
        let size = size.clamp(8.0, 32.0);
        if size == self.text.cell.size {
            return;
        }
        self.text.set_size(size);
        self.session.set_cell_size(self.text.cell.width, self.text.cell.height);
        let grid = grid_size(self.width, self.height, &self.text);
        self.session.resize(grid);
        self.session.reflow();
        self.show_toast(&format!("Font size {size:.0}"));
    }

    fn key(&mut self, key: KeyEvent) {
        let named = keys::named(&key);
        // Alt (Haiku's Command key) shortcuts work in every mode.
        if key.cmd() && !key.ctrl() {
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
                    self.copy(key.shift());
                    true
                }
                (_, Some('v')) => {
                    self.paste();
                    true
                }
                (_, Some('k')) => {
                    self.clear_blocks();
                    true
                }
                (_, Some('q' | 'w')) => {
                    self.close();
                    true
                }
                (_, Some('=' | '+')) => {
                    self.change_font_size(self.text.cell.size + 1.0);
                    true
                }
                (_, Some('-')) => {
                    self.change_font_size(self.text.cell.size - 1.0);
                    true
                }
                (_, Some('0')) => {
                    self.change_font_size(DEFAULT_FONT_SIZE);
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
            if handled {
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
                self.session.write(&bytes);
                self.selection = None;
                self.pin_to_bottom();
            }
            return;
        }

        match self.session.phase {
            Phase::Ready | Phase::Continuation => self.editor_key(key, named),
            Phase::Exited => {
                if named == Some(Named::Enter) {
                    self.restart();
                }
            }
            _ => {
                if key.ctrl() && key.base_char() == Some('c') {
                    self.session.interrupt();
                }
            }
        }
    }

    fn restart(&mut self) {
        let size = grid_size(self.width, self.height, &self.text);
        self.session = Session::new(size);
        self.session.set_cell_size(self.text.cell.width, self.text.cell.height);
        if let Err(error) = self.session.start(self.sender.clone(), self.credits.clone()) {
            self.show_toast(&format!("Couldn't start the shell: {error}"));
        }
        self.selection = None;
        self.selected = None;
    }

    fn editor_key(&mut self, key: KeyEvent, named: Option<Named>) {
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
        let suggestion = self.editor.suggestion(&self.history).map(str::to_owned);
        let word = key.ctrl();
        match named {
            Some(Named::Enter) => {
                if shift || key.option() {
                    self.editor.insert("\n");
                } else {
                    self.submit();
                }
            }
            Some(Named::Backspace) => {
                if word {
                    self.editor.delete_word_back()
                } else {
                    self.editor.backspace()
                }
            }
            Some(Named::Delete) => self.editor.delete(),
            Some(Named::Left) => self.editor.left(word, shift),
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
                    self.editor.browse(&self.history, true);
                }
            }
            Some(Named::Down) => {
                if !self.editor.vertical(true, shift) {
                    self.editor.browse(&self.history, false);
                }
            }
            Some(Named::Tab) => self.tab(),
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
                        Some('r') => self.open_history_menu(),
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
                    // Unbound Alt combinations: nothing at the prompt.
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
                self.open_history_menu();
            } else {
                self.completion =
                    complete::complete(&self.editor.text, self.editor.cursor, &self.session.cwd, &mut self.commands)
                        .filter(|c| c.candidates.len() > 1);
            }
        }
    }

    fn submit(&mut self) {
        let command = self.editor.text.trim_end().to_owned();
        self.completion = None;
        if command.trim().is_empty() {
            return;
        }
        self.history.push(&command);
        self.editor.take();
        self.session.submit(&command);
        self.selected = None;
        self.selection = None;
        self.pin_to_bottom();
    }

    fn tab(&mut self) {
        let Some(mut completion) =
            complete::complete(&self.editor.text, self.editor.cursor, &self.session.cwd, &mut self.commands)
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

    fn open_history_menu(&mut self) {
        let query = self.editor.text.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let candidates: Vec<Candidate> = self
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

    fn copy(&mut self, command_only: bool) {
        let text = if let Some(text) = self.editor.selected_text().filter(|_| self.session.editor_active()) {
            Some(text.to_owned())
        } else if let Some(selection) = self.selection {
            self.session.blocks.iter().find(|b| b.id == selection.block).map(|block| {
                let (start, end) = normalize(selection);
                block.text_between(start, end)
            })
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
                self.show_toast(&if lines > 1 { format!("Copied {lines} lines") } else { "Copied".to_owned() });
            }
            _ => platform::beep(),
        }
    }

    fn paste(&mut self) {
        let Some(text) = platform::clipboard_get() else { return };
        let text = text.replace("\r\n", "\n");
        if let Some(block) = self.session.active() {
            let bracketed = block.term.mode().contains(TermMode::BRACKETED_PASTE);
            let body = text.replace('\n', "\r");
            if bracketed {
                self.session.write(format!("\x1b[200~{}\x1b[201~", body.replace("\x1b[201~", "")).as_bytes());
            } else {
                self.session.write(body.as_bytes());
            }
        } else if self.session.editor_active() {
            self.editor.insert(&text);
        }
    }

    fn wheel(&mut self, dy: f32, x: f32, y: f32) {
        if let Some(block) = self.session.active() {
            if block.alt_screen() {
                let mode = *block.term.mode();
                if mode.intersects(TermMode::MOUSE_MODE) {
                    if let Some((column, line)) = self.alt_cell(x, y) {
                        let button = if dy < 0.0 { 64 } else { 65 };
                        for _ in 0..(dy.abs().ceil() as usize).max(1) {
                            self.mouse_report(button, column, line, true);
                        }
                    }
                } else if mode.contains(TermMode::ALTERNATE_SCROLL) {
                    let arrow: &[u8] = if mode.contains(TermMode::APP_CURSOR) {
                        if dy < 0.0 {
                            b"\x1bOA"
                        } else {
                            b"\x1bOB"
                        }
                    } else if dy < 0.0 {
                        b"\x1b[A"
                    } else {
                        b"\x1b[B"
                    };
                    let count = (dy.abs() * 3.0).round().max(1.0) as usize;
                    self.session.write(&arrow.repeat(count));
                }
                return;
            }
        }
        self.scroll_by(-dy * 3.0 * self.text.cell.height);
    }

    fn alt_cell(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        let (_, ox, oy) = self.layout.alt?;
        let cell = self.text.cell;
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

    fn mouse_down(&mut self, x: f32, y: f32, buttons: i32, clicks: i32, mods: u32) {
        self.dirty = true;
        let button = if buttons & 2 != 0 {
            2
        } else if buttons & 4 != 0 {
            1
        } else {
            0
        };
        if let Some((index, _, _)) = self.layout.alt {
            let block = &self.session.blocks[index];
            let mode = *block.term.mode();
            if mode.intersects(TermMode::MOUSE_MODE) && mods & platform::mods::SHIFT == 0 {
                if let Some((column, line)) = self.alt_cell(x, y) {
                    self.mouse_report(button, column, line, true);
                }
                return;
            }
            if let Some((column, line)) = self.alt_cell(x, y) {
                let id = block.id;
                self.start_selection(id, line, column, clicks);
            }
            return;
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
            self.place_editor_cursor(x, y, mods & platform::mods::SHIFT != 0);
            self.selected = None;
            return;
        }
        let hit =
            self.layout.blocks.iter().find(|b| b.rect.contains(x, y)).map(|b| (b.id, b.output_x, b.output_y, b.lines));
        let Some((id, ox, oy, lines)) = hit else {
            self.selected = None;
            self.selection = None;
            return;
        };
        let cell = self.text.cell;
        if button == 0 && lines > 0 && y >= oy && y < oy + lines as f32 * cell.height {
            let line = ((y - oy) / cell.height) as usize;
            let column = ((x - ox) / cell.width).max(0.0) as usize;
            self.start_selection(id, line.min(lines - 1), column.min(self.session.size.columns - 1), clicks);
            self.selected = Some(id);
        } else {
            self.selection = None;
            self.selected = if self.selected == Some(id) && button == 0 { None } else { Some(id) };
        }
    }

    fn start_selection(&mut self, block: u64, line: usize, column: usize, clicks: i32) {
        let mode = match clicks {
            2 => SelectMode::Word,
            n if n >= 3 => SelectMode::Line,
            _ => SelectMode::Char,
        };
        self.selection = Some(Selection { block, anchor: (line, column), head: (line, column), mode });
        self.dragging = true;
    }

    fn mouse_moved(&mut self, x: f32, y: f32, buttons: i32) {
        // Pointer shape: an arrow over chrome, the I-beam over text.
        let over_text = self.layout.alt.is_some()
            || self.layout.input.contains(x, y)
            || self.layout.blocks.iter().any(|b| b.rect.contains(x, y) && y >= b.output_y);
        self.set_cursor(if over_text { Cursor::IBeam } else { Cursor::Arrow });

        if let Some((index, _, _)) = self.layout.alt {
            let mode = *self.session.blocks[index].term.mode();
            if buttons != 0 && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                if let Some((column, line)) = self.alt_cell(x, y) {
                    self.mouse_report(32, column, line, true);
                }
                return;
            }
        }
        if !self.dragging || buttons == 0 {
            return;
        }
        let Some(mut selection) = self.selection else { return };
        let cell = self.text.cell;
        let (line, column) = if let Some((_, ox, oy)) = self.layout.alt {
            let column = ((x - ox) / cell.width).max(0.0) as usize;
            let line = ((y - oy) / cell.height).max(0.0) as usize;
            (line.min(self.session.size.lines - 1), column)
        } else {
            let Some(layout) = self.layout.blocks.iter().find(|b| b.id == selection.block) else { return };
            if layout.lines == 0 {
                return;
            }
            let line = ((y - layout.output_y) / cell.height).floor().clamp(0.0, (layout.lines - 1) as f32) as usize;
            let column = ((x - layout.output_x) / cell.width).max(0.0) as usize;
            (line, column)
        };
        selection.head = (line, column.min(self.session.size.columns - 1));
        self.selection = Some(selection);
        self.dirty = true;
    }

    fn mouse_up(&mut self, x: f32, y: f32) {
        if let Some((index, _, _)) = self.layout.alt {
            let mode = *self.session.blocks[index].term.mode();
            if mode.intersects(TermMode::MOUSE_MODE) {
                if let Some((column, line)) = self.alt_cell(x, y) {
                    self.mouse_report(0, column, line, false);
                }
            }
        }
        self.dragging = false;
        if let Some(selection) = self.selection {
            if selection.mode == SelectMode::Char && selection.anchor == selection.head {
                self.selection = None;
            }
        }
        self.dirty = true;
    }

    fn place_editor_cursor(&mut self, x: f32, y: f32, extend: bool) {
        if !self.session.editor_active() {
            return;
        }
        let cell = self.text.cell;
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

    // ---- drawing ------------------------------------------------------

    fn blink_on(&self) -> bool {
        let elapsed = Instant::now().duration_since(self.blink_epoch);
        (elapsed.as_millis() / BLINK.as_millis()).is_multiple_of(2)
    }

    fn render(&mut self) {
        crate::trace("render: begin");
        self.dirty = false;
        self.animation_at = self.animation_deadline();
        let (width, height) = (self.width as f32, self.height as f32);
        self.scene.reset(width, height, self.theme.background);
        self.layout = Layout::default();
        let alt = self.session.active().is_some_and(|b| b.alt_screen());
        if alt {
            self.draw_alt();
        } else {
            self.draw_main();
        }
        self.draw_toast();
        if self.bell.is_some() {
            self.scene.set_layer(LAYER_OVERLAY);
            let edge = Rect::new(0.0, 0.0, width, height);
            self.scene.quad(Quad::new(edge, Color::TRANSPARENT).border(3.0, self.theme.warning.alpha(0.7)));
        }
        crate::trace("render: scene built");
        match self.gpu.render(&self.scene, &mut self.text.atlas) {
            Ok(pixels) => {
                if let Some(path) = std::env::var_os("TEMINO_CAPTURE") {
                    write_ppm(&path, pixels, self.width, self.height);
                }
                platform::present(pixels, self.width, self.height)
            }
            Err(error) => {
                crate::fatal(&format!("rendering failed: {error}"));
                self.close();
            }
        }
    }

    fn selection_span(&self, id: u64) -> Option<Span> {
        let selection = self.selection.filter(|s| s.block == id)?;
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

    fn draw_alt(&mut self) {
        let Some(index) = self.session.blocks.iter().rposition(|b| b.running() && b.kind == BlockKind::Command) else {
            return;
        };
        let id = self.session.blocks[index].id;
        let selection = self.selection_span(id);
        let blink = self.blink_on();
        let (x, y) = (PAD_X, ALT_PAD_Y);
        self.layout.alt = Some((index, x, y));
        let lines = self.session.blocks[index].term.grid().screen_lines();
        let App { scene, text, theme, session, focused, .. } = self;
        let view = GridView {
            block: &session.blocks[index],
            x,
            y,
            first: 0,
            last: lines,
            cursor: true,
            focused: *focused,
            selection,
            blink_on: blink,
        };
        ui::draw_grid(scene, text, theme, view);
    }

    fn block_height(&mut self, index: usize) -> (f32, usize) {
        let cell = self.text.cell;
        let columns = self.session.size.columns.max(1);
        let block = &mut self.session.blocks[index];
        let command_lines = if block.kind == BlockKind::Command {
            block.command.split('\n').map(|line| line.chars().count().div_ceil(columns).max(1)).sum()
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

    fn input_height(&self) -> f32 {
        let lines = if self.session.editor_active() { self.editor.line_count().min(MAX_EDITOR_LINES) } else { 1 };
        INPUT_PAD + CHIP_HEIGHT + 8.0 + lines as f32 * self.text.cell.height + INPUT_PAD + 2.0
    }

    fn draw_main(&mut self) {
        let (width, height) = (self.width as f32, self.height as f32);
        let input_height = self.input_height();
        let welcome_height = if self.session.blocks.is_empty() { self.welcome_height() } else { 0.0 };

        let mut heights = Vec::with_capacity(self.session.blocks.len());
        let mut total = welcome_height;
        for index in 0..self.session.blocks.len() {
            let h = self.block_height(index);
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

        self.scene.push_clip(Rect::new(0.0, 0.0, width, input_y));
        if welcome_height > 0.0 {
            self.draw_welcome(top);
        }
        let mut y = top + welcome_height;
        for (index, (block_height, output)) in heights.into_iter().enumerate() {
            if y + block_height > 0.0 && y < input_y {
                self.draw_block(index, y, block_height, output);
            }
            y += block_height;
        }
        self.scene.pop_clip();

        if total > list_max && list_max > 40.0 {
            let thumb = (list_max / total * list_max).max(24.0);
            let position = (1.0 - self.scroll / max_scroll.max(1.0)) * (list_max - thumb);
            let bar = Rect::new(width - 6.0, position + 2.0, 3.0, thumb - 4.0);
            self.scene.quad(Quad::new(bar, self.theme.text_faint.alpha(0.45)).radius(1.5));
        }
        self.draw_input(input_y, input_height);
    }

    /// Whether the window is wide enough for the ASCII-art banner.
    fn banner_fits(&self) -> bool {
        self.session.size.columns >= BANNER[0].chars().count() + 2
    }

    fn welcome_height(&self) -> f32 {
        let title = if self.banner_fits() { BANNER.len() as f32 * self.text.cell.height } else { 30.0 };
        24.0 + title + 30.0 + 2.0 * 28.0 + 12.0
    }

    fn draw_welcome(&mut self, top: f32) {
        let banner = self.banner_fits();
        let App { scene, text, theme, gpu, .. } = self;
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
        let tagline = "A modern GPU-accelerated terminal for Haiku";
        let width = text.draw_text(scene, tagline, x, y + 22.0, UI, 13.0, theme.text_dim);
        let device = format!("  ·  Vulkan on {}", gpu.device_name);
        text.draw_text(scene, &device, x + width, y + 22.0, UI, 13.0, theme.text_faint);
        let y = y + 36.0;
        let rows: [&[(&str, &str)]; 2] = [
            &[
                ("Enter", "run"),
                ("Shift+Enter", "new line"),
                ("Tab", "complete"),
                ("↑ ↓", "history"),
                ("Ctrl+R", "search history"),
            ],
            &[
                ("Alt+K", "clear"),
                ("Alt+↑ ↓", "select blocks"),
                ("Alt+C", "copy"),
                ("Alt+V", "paste"),
                ("Alt+ + −", "font size"),
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

    fn draw_block(&mut self, index: usize, y: f32, block_height: f32, output: usize) {
        let width = self.width as f32;
        let cell = self.text.cell;
        let blink = self.blink_on();
        let id = self.session.blocks[index].id;
        let selection = self.selection_span(id);
        let selected = self.selected == Some(id);
        let focused = self.focused;
        let user_columns = self.session.size.columns.max(1);
        let App { scene, text, theme, session, layout, .. } = self;
        let block = &session.blocks[index];
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
                    Some(code) => (format!("{}  exit {code}", ui::format_duration(block.duration())), theme.error),
                }
            };
            let status_width = text.measure(&status, UI, 12.0);
            text.draw_text(scene, &status, width - PAD_X - status_width, baseline, UI, 12.0, color);
        }

        // The command, wrapped to the grid width.
        let mut line_y = y + BLOCK_PAD_TOP + META_HEIGHT;
        if block.kind == BlockKind::Command {
            for line in block.command.split('\n') {
                let chars: Vec<char> = line.chars().collect();
                for chunk in chars.chunks(user_columns).chain(chars.is_empty().then_some(&[][..])) {
                    let s: String = chunk.iter().collect();
                    text.draw_mono(scene, &s, PAD_X, line_y, Style::BOLD, theme.text);
                    line_y += cell.height;
                }
            }
        }

        let output_y = line_y + OUTPUT_GAP;
        if output > 0 {
            // Only the visible slice of long output is drawn.
            let first = ((-output_y) / cell.height).floor().max(0.0) as usize;
            let last = (((scene.height - output_y) / cell.height).ceil().max(0.0) as usize).min(output);
            if first < last {
                let view = GridView {
                    block,
                    x: PAD_X,
                    y: output_y + first as f32 * cell.height,
                    first,
                    last,
                    cursor: block.running(),
                    focused,
                    selection,
                    blink_on: blink,
                };
                ui::draw_grid(scene, text, theme, view);
            }
        }
        scene.fill(Rect::new(0.0, y + block_height - 1.0, width, 1.0), theme.divider);
        layout.blocks.push(BlockLayout { id, rect, output_x: PAD_X, output_y, lines: output });
    }

    fn draw_input(&mut self, y: f32, height: f32) {
        let width = self.width as f32;
        let cell = self.text.cell;
        let blink = self.blink_on() && self.focused;
        let phase = self.session.phase;
        let App { scene, text, theme, session, editor, history, commands, layout, completion, user_host, .. } = self;
        let panel = Rect::new(0.0, y, width, height);
        layout.input = panel;
        scene.fill(panel, theme.surface);
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
            _ => theme.accent,
        };
        let prompt = if text.has_glyph('❯', 0) { "❯" } else { ">" };
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
                let message = "Running · keystrokes go to the program · Ctrl+C interrupts · Alt+C copies";
                status(scene, text, message, theme.text_faint)
            }
            Phase::Prompt | Phase::Submitted => {}
            Phase::Ready | Phase::Continuation => {
                let first_line = editor.position(editor.cursor).0.saturating_sub(MAX_EDITOR_LINES - 1);
                if editor.text.is_empty() {
                    let hint = if phase == Phase::Continuation {
                        "The command needs more input; Ctrl+C cancels it"
                    } else {
                        "Type a command · Tab completes · ↑ history · Shift+Enter new line"
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
                            editor.text.split('\n').nth(line).map_or(0, |l| l.chars().count()) + 1
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
                let spans = ui::highlight(&editor.text, editor.cursor, commands, theme);
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
                    if line >= first_line && line < first_line + MAX_EDITOR_LINES {
                        let cx = text_x + column as f32 * cell.width;
                        let cy = editor_y + (line - first_line) as f32 * cell.height;
                        text.draw_cell(scene, c, cx, cy, Style::default(), colors[offset], 1);
                    }
                    column += 1;
                }
                if let Some(rest) = editor.suggestion(history) {
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
                if blink {
                    let (cl, cc) = editor.position(editor.cursor);
                    if cl >= first_line {
                        let cy = editor_y + (cl - first_line) as f32 * cell.height;
                        scene.fill(
                            Rect::new(text_x + cc as f32 * cell.width - 1.0, cy + 1.0, 2.0, cell.height - 2.0),
                            theme.cursor,
                        );
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
                        text_x + word_column as f32 * cell.width,
                        editor_y,
                    );
                }
            }
        }
    }

    fn draw_toast(&mut self) {
        let Some((message, _)) = &self.toast else { return };
        let App { scene, text, theme, layout, width, .. } = self;
        scene.set_layer(LAYER_OVERLAY);
        let w = text.measure(message, UI, 12.0) + 24.0;
        let bottom = if layout.input.h > 0.0 { layout.input.y } else { scene.height };
        let rect = Rect::new(*width as f32 - w - 14.0, bottom - 40.0, w, 28.0);
        scene.shadow(rect, Color::hex(0x000000).alpha(0.5), 14.0, 8.0);
        scene.quad(Quad::new(rect, theme.raised).radius(14.0).border(1.0, theme.divider));
        text.draw_text(scene, message, rect.x + 12.0, rect.y + 18.0, UI, 12.0, theme.text);
        scene.set_layer(LAYER_CONTENT);
    }
}

fn draw_menu(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    layout: &mut Layout,
    completion: &Completion,
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
    let width = (label_width + 90.0).clamp(220.0, scene.width - 2.0 * PAD_X);
    let footer = if completion.candidates.len() > rows { 18.0 } else { 0.0 };
    let height = rows as f32 * row_height + 8.0 + footer;
    let x = (x - 10.0).min(scene.width - width - PAD_X).max(PAD_X);
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

/// TEMINO_CAPTURE: saves each frame, straight from the GPU readback, for
/// screenshots that no other window or the pointer can get into.
fn write_ppm(path: &std::ffi::OsStr, bgra: &[u8], width: u32, height: u32) {
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    ppm.reserve(bgra.len() / 4 * 3);
    for pixel in bgra.chunks_exact(4) {
        ppm.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    let _ = std::fs::write(path, ppm);
}

fn normalize(selection: Selection) -> ((usize, usize), (usize, usize)) {
    if selection.anchor <= selection.head {
        (selection.anchor, selection.head)
    } else {
        (selection.head, selection.anchor)
    }
}

fn grid_size(width: u32, height: u32, text: &TextSystem) -> GridSize {
    let cell = text.cell;
    GridSize {
        columns: (((width as f32 - 2.0 * PAD_X) / cell.width).floor() as usize).max(10),
        lines: (((height as f32 - 2.0 * ALT_PAD_Y) / cell.height).floor() as usize).max(3),
    }
}

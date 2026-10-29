//! The core thread: owns the renderer and the window's tabs, each a tree
//! of split panes; routes window events and pty output to panes, and draws
//! frames on demand.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::agent::{self, AgentState};
use crate::complete::Commands;
use crate::editor::History;
use crate::gpu::Gpu;
use crate::keys::Named;
use crate::pane::{AgentInfo, Ctx, MarkNote, Pane};
use crate::platform::{self, buttons, Cursor, KeyEvent, UiEvent};
use crate::scene::{Quad, Rect, Scene, LAYER_CONTENT, LAYER_OVERLAY};
use crate::settings::{self, Action, Field};
use crate::shells::{self, KINDS};
use crate::text::{Style, TextSystem, UI, UI_BOLD};
use crate::theme::{Color, Theme, BACKGROUNDS, OPACITIES, SURFACE_LIFT};
use crate::ui;
use crate::update::{self, Release};
use crate::Msg;

/// The terminal font size at zoom 1, which every UI size is designed at.
const BASE_FONT_SIZE: f32 = 14.0;
/// The terminal font size at 100%: the window draws a little larger than
/// its design size.
const DEFAULT_FONT_SIZE: f32 = 15.4;
const TAB_BAR_HEIGHT: f32 = 34.0;
const TAB_MAX_WIDTH: f32 = 220.0;
const SIDEBAR_WIDTH: f32 = 200.0;
const SIDEBAR_MIN: f32 = 140.0;
const SIDEBAR_MAX: f32 = 420.0;
/// Dragged narrower than this, the sidebar snaps to compact.
const SIDEBAR_SNAP: f32 = 100.0;
/// The compact sidebar: tab numbers only.
const SIDEBAR_COMPACT: f32 = 54.0;
const SIDEBAR_ROW: f32 = 40.0;
/// Sidebar rows read a little larger than the strip's tabs.
const SIDEBAR_FONT: f32 = 13.5;
/// An agent waiting on you, and one that finished a turn you haven't seen.
const AGENT_WAITING: u32 = 0xf0a35e;
const AGENT_DONE: u32 = 0x8ccf7e;
/// The bar over a pane with a coding agent in it.
const AGENT_HEADER: f32 = 26.0;
/// The extended sidebar's line per pane, under each tab's title.
const SIDEBAR_LINE: f32 = 18.0;
/// How long the pointer rests on a tab before its folder shows beside it.
const TAB_HINT_DELAY: Duration = Duration::from_millis(600);

/// Where the tabs are listed: down the left side (the default) or across
/// the top (`TEMINO_TABS=top`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum TabStrip {
    Left,
    Top,
}
/// Panes sit this far apart; the divider line is drawn in the gap.
const GAP: f32 = 1.0;
/// How far from a divider a press still grabs it.
const GRAB: f32 = 4.0;
const MIN_PANE: f32 = 120.0;
/// How far a press on a tab moves before it becomes a drag.
const DRAG_SLOP: f32 = 4.0;
/// How long a reordered tab takes to slide into its new place.
const TAB_SLIDE: Duration = Duration::from_millis(160);
const MENU_WIDTH: f32 = 200.0;
const MENU_ROW: f32 = 26.0;

/// The colors a tab can be marked with, from its menu.
const TAB_COLORS: [u32; 6] = [0xf2737a, 0xf0a35e, 0xe5c76b, 0x8ccf7e, 0x67b0e8, 0xc47fd5];

/// What a tab is waiting on you for, set by hand so a glance at the list
/// says what to do with it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mark {
    /// Deal with it first.
    Important,
    /// Done for now, held up by something else.
    Waiting,
    /// Set aside to come back to.
    Later,
}

impl Mark {
    const ALL: [Mark; 3] = [Mark::Important, Mark::Waiting, Mark::Later];

    fn label(self) -> &'static str {
        match self {
            Mark::Important => "Important",
            Mark::Waiting => "Waiting",
            Mark::Later => "Later",
        }
    }

    fn color(self, theme: &Theme) -> Color {
        match self {
            Mark::Important => theme.error,
            Mark::Waiting => theme.warning,
            Mark::Later => theme.text_faint,
        }
    }

    /// What it means, for the model writing a recap.
    fn meaning(self) -> &'static str {
        match self {
            Mark::Important => "important, to deal with first",
            Mark::Waiting => "waiting: done for now and held up by something else",
            Mark::Later => "set aside to come back to later",
        }
    }

    /// Set aside: ⌘` passes it by, and typing in it clears the mark.
    fn aside(self) -> bool {
        self != Mark::Important
    }

    /// How it's written in the saved tabs file.
    fn code(self) -> &'static str {
        match self {
            Mark::Important => "important",
            Mark::Waiting => "waiting",
            Mark::Later => "later",
        }
    }

    fn from_code(code: &str) -> Option<Mark> {
        Mark::ALL.into_iter().find(|mark| mark.code() == code)
    }
}

/// Separates the panes of a tab in the saved tabs file.
const LAYOUT_SEPARATOR: char = '\u{1f}';
/// The Settings tab's one "pane": no shell, the settings page instead.
/// Shell panes count up from 1.
const SETTINGS: u64 = 0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Axis {
    /// Side by side, divided by a vertical line.
    Row,
    /// Stacked, divided by a horizontal line.
    Column,
}

enum Node {
    Leaf(u64),
    Split { axis: Axis, ratio: f32, first: Box<Node>, second: Box<Node> },
}

impl Node {
    fn leaves(&self, out: &mut Vec<u64>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
        }
    }

    fn contains(&self, id: u64) -> bool {
        match self {
            Node::Leaf(leaf) => *leaf == id,
            Node::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// Replaces leaf `id` with a split of it and `new`.
    fn split(&mut self, id: u64, new: u64, axis: Axis) -> bool {
        match self {
            Node::Leaf(leaf) if *leaf == id => {
                *self = Node::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(id)),
                    second: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => first.split(id, new, axis) || second.split(id, new, axis),
        }
    }

    /// Takes leaf `id` out; its sibling takes the parent's place. Returns
    /// false when `id` is this whole node (the caller drops it).
    fn remove(&mut self, id: u64) -> bool {
        let Node::Split { first, second, .. } = self else { return false };
        let keep = if matches!(**first, Node::Leaf(leaf) if leaf == id) {
            std::mem::replace(&mut **second, Node::Leaf(0))
        } else if matches!(**second, Node::Leaf(leaf) if leaf == id) {
            std::mem::replace(&mut **first, Node::Leaf(0))
        } else {
            return first.remove(id) || second.remove(id);
        };
        *self = keep;
        true
    }

    /// Lays the tree out in `rect`: each pane's rectangle, and the dividers
    /// (with the path to their split, false = first child).
    fn layout(&self, rect: Rect, path: &mut Vec<bool>, panes: &mut Vec<(u64, Rect)>, dividers: &mut Vec<Divider>) {
        match self {
            Node::Leaf(id) => panes.push((*id, rect)),
            Node::Split { axis, ratio, first, second } => {
                let (a, b, line) = split_rect(rect, *axis, *ratio);
                dividers.push(Divider { line, axis: *axis, parent: rect, path: path.clone() });
                path.push(false);
                first.layout(a, path, panes, dividers);
                path.pop();
                path.push(true);
                second.layout(b, path, panes, dividers);
                path.pop();
            }
        }
    }

    fn at_path(&mut self, path: &[bool]) -> Option<&mut Node> {
        match path.split_first() {
            None => Some(self),
            Some((&side, rest)) => match self {
                Node::Split { first, second, .. } => (if side { second } else { first }).at_path(rest),
                Node::Leaf(_) => None,
            },
        }
    }
}

fn split_rect(rect: Rect, axis: Axis, ratio: f32) -> (Rect, Rect, Rect) {
    match axis {
        Axis::Row => {
            let w = ((rect.w - GAP) * ratio).round();
            let a = Rect::new(rect.x, rect.y, w, rect.h);
            let line = Rect::new(rect.x + w, rect.y, GAP, rect.h);
            let b = Rect::new(rect.x + w + GAP, rect.y, rect.w - w - GAP, rect.h);
            (a, b, line)
        }
        Axis::Column => {
            let h = ((rect.h - GAP) * ratio).round();
            let a = Rect::new(rect.x, rect.y, rect.w, h);
            let line = Rect::new(rect.x, rect.y + h, rect.w, GAP);
            let b = Rect::new(rect.x, rect.y + h + GAP, rect.w, rect.h - h - GAP);
            (a, b, line)
        }
    }
}

#[derive(Clone)]
struct Divider {
    line: Rect,
    axis: Axis,
    /// The area the split divides, to turn the pointer into a ratio.
    parent: Rect,
    path: Vec<bool>,
}

impl Divider {
    fn grab_area(&self) -> Rect {
        match self.axis {
            Axis::Row => Rect::new(self.line.x - GRAB, self.line.y, self.line.w + 2.0 * GRAB, self.line.h),
            Axis::Column => Rect::new(self.line.x, self.line.y - GRAB, self.line.w, self.line.h + 2.0 * GRAB),
        }
    }
}

struct Tab {
    root: Node,
    focus: u64,
    /// The focused pane fills the tab (Command+Enter).
    zoomed: bool,
    /// Set from the tab's menu; otherwise the tab shows its pane's title.
    name: Option<String>,
    /// An entry of TAB_COLORS.
    color: Option<usize>,
    /// Where its row was last drawn, and where it slides from after the
    /// tabs are reordered.
    shown: Option<(f32, f32)>,
    slide: Option<((f32, f32), Instant)>,
    mark: Option<Mark>,
    /// When the mark was set, this run; a restored mark doesn't know.
    marked_at: Option<Instant>,
}

/// What a row of the tab menu does.
#[derive(Clone, Copy)]
enum MenuAction {
    Rename,
    Color(Option<usize>),
    Mark(Mark),
    Close,
    CloseOthers,
    // A pane's menu: these act on the focused pane.
    Copy,
    Paste,
    Clear,
    SplitRight,
    SplitDown,
    ClosePane,
}

/// A tab's menu, or with `pane` the focused pane's (a right click in
/// it), open at a point.
struct TabMenu {
    tab: usize,
    x: f32,
    y: f32,
    pane: bool,
}

/// The release the update check found, and its card.
struct UpdateOffer {
    release: Release,
    /// The card is showing; "Later" hides it and leaves the pill.
    open: bool,
    installing: bool,
}

/// What a click on the update pill or card does.
#[derive(Clone, Copy)]
enum UpdateAction {
    Toggle,
    Skip,
    Later,
    Install,
    Notes,
}

/// A tab being renamed in place.
struct Rename {
    tab: usize,
    name: String,
    /// The whole name is selected: typing replaces it.
    selected: bool,
}

/// How a tab is drawn in the tab list.
struct TabView {
    title: String,
    panes: usize,
    color: Option<Color>,
    /// The name being typed, and whether it is all selected, while renaming.
    editing: Option<(String, bool)>,
    /// The most urgent state of the agents in the tab's panes.
    agent: Option<AgentState>,
    /// In the extended sidebar: a line on what each of its panes is doing.
    lines: Vec<PaneLine>,
    /// Its focused pane's folder, in full.
    path: String,
    mark: Option<Mark>,
}

/// One pane's line in the extended sidebar.
struct PaneLine {
    id: u64,
    agent: Option<AgentState>,
    /// Something runs in it.
    busy: bool,
    /// Idle, and its last command failed.
    failed: bool,
    text: String,
}

enum Pointer {
    Idle,
    /// A press went to this pane; drags and the release follow it.
    Pane(u64),
    /// Resizing the split at this path.
    Divider(Divider),
    /// Dragging the sidebar's edge.
    Sidebar,
    /// Pressed on a tab: a click, or a drag that moves it in the list.
    Tab {
        index: usize,
        x: f32,
        y: f32,
        dragging: bool,
    },
}

pub struct App {
    gpu: Gpu,
    scene: Scene,
    ctx: Ctx,
    panes: Vec<Pane>,
    tabs: Vec<Tab>,
    active: usize,
    /// The pane focused before this one, for Command+` with nothing waiting.
    last_focus: Option<u64>,
    strip: TabStrip,
    /// The sidebar's width when not compact, and whether it is compact;
    /// or extended, its rows grown to say what each pane is doing.
    sidebar_width: f32,
    compact: bool,
    extended: bool,
    /// Command+B's choice to show or hide the tab list; until then it shows
    /// once there is a second tab. Hidden, tabs still switch by key.
    tab_list: Option<bool>,
    next_id: u64,
    /// The window's size in logical pixels, and physical pixels per logical one.
    window: (f32, f32),
    scale: f32,
    /// Command+plus/minus: everything is drawn this much larger.
    zoom: f32,
    /// The window's size in zoomed pixels, which everything is laid out in.
    width: f32,
    height: f32,
    focused: bool,
    dirty: bool,
    title: String,
    cursor: Cursor,
    quitting: bool,
    /// When the frame on screen goes stale on its own (spinner, blink).
    animation_at: Option<Instant>,
    pointer: Pointer,
    /// Per frame: the tab bar's hit areas and the active tab's dividers.
    tab_hits: Vec<(Rect, usize)>,
    close_hits: Vec<(Rect, usize)>,
    new_tab_hit: Rect,
    collapse_hit: Rect,
    extend_hit: Rect,
    /// The extended sidebar's pane lines and the agent headers, by pane.
    pane_hits: Vec<(Rect, usize, u64)>,
    more_hits: Vec<(Rect, usize)>,
    menu_hits: Vec<(Rect, MenuAction)>,
    menu_rect: Rect,
    dividers: Vec<Divider>,
    tab_menu: Option<TabMenu>,
    renaming: Option<Rename>,
    /// The Settings text field being typed in, its text, and whether
    /// that's all selected.
    field: Option<(Field, String, bool)>,
    /// Whether panes running an agent (or a listed program) get a header.
    headers: bool,
    hover_tab: Option<usize>,
    /// When the pointer came onto the hovered tab, for its path's label.
    hover_at: Instant,
    mouse: (f32, f32),
    /// What an input method is composing, drawn at the cursor.
    preedit: String,
    /// The caret last reported to the platform, in window pixels.
    caret_sent: Option<[i32; 3]>,
    /// The pane installing a shell from Settings ▸ Shell, and which.
    installing: Option<(u64, shells::Kind)>,
    /// The Settings tab's page, and what it shows that the theme doesn't.
    settings: settings::Page,
    background: usize,
    opacity: usize,
    blur: bool,
    /// Which of KINDS are installed, as of the Settings tab opening.
    installed: [bool; KINDS.len()],
    update: Option<UpdateOffer>,
    /// Per frame: the update pill and card's hit areas, and the card.
    update_hits: Vec<(Rect, UpdateAction)>,
    update_card: Rect,
    /// Quitting to start the installed update.
    relaunch: bool,
    /// `TEMINO_RECAP_TEST=<seconds>`: show the focused pane's recap this
    /// long after launch, as Command+I would, for screenshots.
    recap_test: Option<Instant>,
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
                    Msg::Pty(..) => "loop: pty data",
                    Msg::ShellExited(..) => "loop: shell exited",
                    Msg::Notice(..) => "loop: notice",
                    Msg::Update(..) => "loop: update",
                    Msg::Git(..) => "loop: git",
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
    let mut text = TextSystem::new(BASE_FONT_SIZE);
    let theme = Theme::dark();
    let mut scene = Scene::new();
    let mut slowest = 0.0f64;
    for round in 0..frames {
        let frame = Instant::now();
        scene.reset(width as f32, height as f32, 1.0, theme.background);
        let card = Rect::new(16.0, 16.0, 448.0, 128.0);
        scene.shadow(card, Color::hex(0x000000).alpha(0.6), 10.0, 8.0);
        scene.quad(Quad::new(card, theme.surface).radius(10.0).border(1.0, theme.divider));
        text.draw_text(&mut scene, "Temino self-test", 32.0, 52.0, UI_BOLD, 20.0, theme.text);
        text.draw_mono(
            &mut scene,
            "ls -la ~/config ─┼─ ✓ 中文 🌞🐱👌🚀🔥🎉",
            32.0,
            70.0,
            Style::default(),
            theme.success,
        );
        scene.fill(Rect::new(32.0, 110.0, 120.0, 18.0), theme.accent);
        match gpu.render(&scene, &mut text.atlas, &mut text.color_atlas, true) {
            Ok(Some(pixels)) => {
                let ms = frame.elapsed().as_secs_f64() * 1000.0;
                slowest = if round > 0 { slowest.max(ms) } else { slowest };
                if round < 3 || round % 100 == 0 {
                    println!("frame {round}: {ms:.2} ms");
                }
                if round + 1 == frames {
                    println!("{frames} frames, slowest after the first {slowest:.2} ms");
                    write_ppm(std::ffi::OsStr::new(path), pixels, width, height);
                    println!("wrote {path}");
                }
            }
            Ok(None) => unreachable!("a readback always returns the frame"),
            Err(error) => {
                eprintln!("selftest: {error}");
                return 1;
            }
        }
    }
    0
}

fn host_name() -> Option<String> {
    #[cfg(not(target_os = "haiku"))]
    {
        let mut buffer = [0u8; 256];
        if unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len()) } == 0 {
            let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
            let name = String::from_utf8_lossy(&buffer[..end]).into_owned();
            return Some(name.trim_end_matches(".local").to_owned());
        }
    }
    std::fs::read_to_string("/boot/system/settings/network/hostname").ok()
}

fn user_host() -> String {
    let user = std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(platform::user_name)
        .unwrap_or_else(|| "user".into());
    let host = host_name()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| platform::OS_NAME.to_lowercase());
    format!("{user}@{host}")
}

/// `background=<name>` in the settings file, as an index into BACKGROUNDS
/// (the first when unset or unknown).
fn load_background() -> usize {
    crate::setting("background").and_then(|name| BACKGROUNDS.iter().position(|(n, _)| *n == name)).unwrap_or(0)
}

/// `opacity=<percent>` in the settings file, as an index into OPACITIES
/// (fully opaque when unset, unknown, or off macOS, whose windows are the
/// only ones that show through).
fn load_opacity() -> usize {
    crate::setting("opacity")
        .filter(|_| cfg!(target_os = "macos"))
        .and_then(|value| OPACITIES.iter().position(|o| o.to_string() == value))
        .unwrap_or(0)
}

impl App {
    fn new(sender: Sender<Msg>, width: u32, height: u32) -> Result<App, String> {
        let gpu = Gpu::new()?;
        let text = TextSystem::new(BASE_FONT_SIZE);
        crate::trace("app: fonts ready");
        let background = load_background();
        let opacity = load_opacity();
        let theme =
            Theme::dark().with_background(BACKGROUNDS[background].1).with_opacity(OPACITIES[opacity] as f32 / 100.0);
        platform::set_background(theme.background.to_rgb8());
        platform::set_background_menu(&BACKGROUNDS.map(|(name, _)| name), background);
        let shell = shells::default_shell();
        let (shell_names, selected) = shells::menu(&shell);
        platform::set_shell_menu(&shell_names, selected);
        let ctx = Ctx {
            text,
            theme,
            history: History::load(),
            commands: Commands::new(),
            sender,
            user_host: user_host(),
            device_name: gpu.device_name.clone(),
            blink_epoch: Instant::now(),
            toast: None,
            shell,
            composing: false,
        };
        let mut app = App {
            gpu,
            scene: Scene::new(),
            ctx,
            panes: Vec::new(),
            tabs: Vec::new(),
            active: 0,
            last_focus: None,
            strip: match std::env::var("TEMINO_TABS").ok().or_else(|| crate::setting("tabs")).as_deref() {
                Some("top") => TabStrip::Top,
                _ => TabStrip::Left,
            },
            sidebar_width: crate::setting("sidebar_width")
                .and_then(|w| w.parse().ok())
                .unwrap_or(SIDEBAR_WIDTH)
                .clamp(SIDEBAR_MIN, SIDEBAR_MAX),
            compact: crate::setting("sidebar").as_deref() == Some("compact"),
            extended: crate::setting("sidebar").as_deref() == Some("extended"),
            tab_list: None,
            next_id: 1,
            window: (width as f32, height as f32),
            scale: 1.0,
            zoom: 1.0,
            width: width as f32,
            height: height as f32,
            focused: true,
            dirty: true,
            title: String::new(),
            cursor: Cursor::IBeam,
            quitting: false,
            animation_at: None,
            pointer: Pointer::Idle,
            tab_hits: Vec::new(),
            close_hits: Vec::new(),
            new_tab_hit: Rect::default(),
            collapse_hit: Rect::default(),
            extend_hit: Rect::default(),
            pane_hits: Vec::new(),
            more_hits: Vec::new(),
            menu_hits: Vec::new(),
            menu_rect: Rect::default(),
            dividers: Vec::new(),
            tab_menu: None,
            renaming: None,
            field: None,
            headers: crate::setting("headers").as_deref() != Some("off"),
            hover_tab: None,
            hover_at: Instant::now(),
            mouse: (0.0, 0.0),
            preedit: String::new(),
            caret_sent: None,
            installing: None,
            settings: settings::Page::default(),
            background,
            opacity,
            blur: crate::setting("blur").as_deref() == Some("on"),
            installed: [false; KINDS.len()],
            update: None,
            update_hits: Vec::new(),
            update_card: Rect::default(),
            recap_test: std::env::var("TEMINO_RECAP_TEST")
                .ok()
                .and_then(|seconds| seconds.parse::<f32>().ok())
                .map(|seconds| Instant::now() + Duration::from_secs_f32(seconds)),
            relaunch: false,
        };
        app.apply_blur();
        app.set_zoom(
            crate::setting("zoom").and_then(|z| z.parse().ok()).unwrap_or(DEFAULT_FONT_SIZE / BASE_FONT_SIZE),
            false,
        );
        app.restore_tabs()?;
        crate::update::check(app.ctx.sender.clone(), false);
        crate::trace("app: shell started");
        Ok(app)
    }

    // ---- panes and tabs -----------------------------------------------

    fn pane(&mut self, id: u64) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    fn focused_id(&self) -> Option<u64> {
        self.tabs.get(self.active).map(|tab| tab.focus)
    }

    /// A new pane, in the focused pane's directory.
    /// A new pane, in `cwd` or else the focused pane's directory.
    fn make_pane_in(&mut self, cwd: Option<String>) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        let cwd = cwd.or_else(|| {
            self.focused_id().and_then(|f| self.panes.iter().find(|p| p.id == f)).map(|p| p.session.cwd.clone())
        });
        let rect = self.content_rect();
        // Only the window's first pane greets with the banner.
        let pane = Pane::new(id, &mut self.ctx, rect, cwd, id == 1)?;
        self.panes.push(pane);
        Ok(id)
    }

    fn make_pane(&mut self) -> Result<u64, String> {
        self.make_pane_in(None)
    }

    fn new_tab(&mut self) -> Result<(), String> {
        let id = self.make_pane()?;
        self.tabs.push(Tab {
            root: Node::Leaf(id),
            focus: id,
            zoomed: false,
            name: None,
            color: None,
            shown: None,
            slide: None,
            mark: None,
            marked_at: None,
        });
        self.switch_tab(self.tabs.len() - 1);
        self.save_tabs();
        Ok(())
    }

    /// Reopens the tabs open at the last quit, each with its panes in their
    /// directories, its name and its color; or one new tab.
    fn restore_tabs(&mut self) -> Result<(), String> {
        let saved = std::fs::read_to_string(tabs_path()).unwrap_or_default();
        let mut active = 0;
        for line in saved.lines() {
            // active?, color, panes, name, mark — separated by tabs.
            let mut fields = line.splitn(5, '\t');
            let (Some(current), Some(color), Some(layout), name, mark) =
                (fields.next(), fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let mut focus = None;
            let root = self.restore_node(&mut layout.split(LAYOUT_SEPARATOR), &mut focus)?;
            let mut leaves = Vec::new();
            root.leaves(&mut leaves);
            self.tabs.push(Tab {
                focus: focus.unwrap_or(leaves[0]),
                root,
                zoomed: false,
                name: name.filter(|n| !n.is_empty()).map(str::to_owned),
                color: color.parse().ok().filter(|&c: &usize| c < TAB_COLORS.len()),
                shown: None,
                slide: None,
                mark: mark.and_then(Mark::from_code),
                marked_at: None,
            });
            if current == "*" {
                active = self.tabs.len() - 1;
            }
        }
        if self.tabs.is_empty() {
            return self.new_tab();
        }
        self.switch_tab(active);
        Ok(())
    }

    /// Rebuilds one node written by `save_node`, opening its panes. A bare
    /// directory (the older format, one pane a tab) is a focused pane.
    fn restore_node<'a>(
        &mut self,
        tokens: &mut impl Iterator<Item = &'a str>,
        focus: &mut Option<u64>,
    ) -> Result<Node, String> {
        let token = tokens.next().unwrap_or("");
        let axis = match token.get(..1) {
            Some("R") => Some(Axis::Row),
            Some("C") => Some(Axis::Column),
            _ => None,
        };
        if let (Some(axis), Ok(ratio)) = (axis, token[1..].parse::<f32>()) {
            let first = Box::new(self.restore_node(tokens, focus)?);
            let second = Box::new(self.restore_node(tokens, focus)?);
            return Ok(Node::Split { axis, ratio: ratio.clamp(0.05, 0.95), first, second });
        }
        let (focused, cwd) = match token.get(..1) {
            Some("F") => (true, &token[1..]),
            Some("P") => (false, &token[1..]),
            _ => (true, token),
        };
        let cwd = Some(cwd.to_owned()).filter(|d| std::path::Path::new(d).is_dir());
        let id = self.make_pane_in(cwd)?;
        if focused && focus.is_none() {
            *focus = Some(id);
        }
        Ok(Node::Leaf(id))
    }

    /// Writes the tabs down for `restore_tabs`.
    fn save_tabs(&self) {
        let lines: String = self
            .tabs
            .iter()
            .enumerate()
            // The Settings tab isn't reopened.
            .filter(|(_, tab)| tab.focus != SETTINGS)
            .map(|(index, tab)| {
                let mut layout = Vec::new();
                self.save_node(&tab.root, tab.focus, &mut layout);
                let layout = layout.join(&LAYOUT_SEPARATOR.to_string());
                let current = if index == self.active { "*" } else { "" };
                let color = tab.color.map(|c| c.to_string()).unwrap_or_default();
                let name = tab.name.as_deref().unwrap_or("");
                let mark = tab.mark.map_or("", Mark::code);
                format!("{current}\t{color}\t{layout}\t{name}\t{mark}\n")
            })
            .collect();
        let _ = std::fs::create_dir_all(crate::settings_dir());
        let _ = std::fs::write(tabs_path(), lines);
    }

    /// A node in prefix order: `R`/`C` and the ratio for a split, then its
    /// two sides; `F` (focused) or `P` and the directory for a pane.
    fn save_node(&self, node: &Node, focus: u64, out: &mut Vec<String>) {
        match node {
            Node::Leaf(id) => {
                let cwd = self.panes.iter().find(|p| p.id == *id).map_or("", |p| p.session.cwd.as_str());
                out.push(format!("{}{cwd}", if *id == focus { 'F' } else { 'P' }));
            }
            Node::Split { axis, ratio, first, second } => {
                out.push(format!("{}{ratio}", if *axis == Axis::Row { 'R' } else { 'C' }));
                self.save_node(first, focus, out);
                self.save_node(second, focus, out);
            }
        }
    }

    fn split(&mut self, axis: Axis) {
        let Some(focus) = self.focused_id().filter(|&id| id != SETTINGS) else { return };
        match self.make_pane() {
            Ok(id) => {
                let tab = &mut self.tabs[self.active];
                tab.root.split(focus, id, axis);
                tab.zoomed = false;
                self.set_focus(id);
                self.save_tabs();
            }
            Err(error) => self.ctx.show_toast(&error),
        }
    }

    /// Closes a pane, its tab when it was the last, the window after that.
    fn close_pane(&mut self, id: u64) {
        if let Some(index) = self.panes.iter().position(|p| p.id == id) {
            let mut pane = self.panes.remove(index);
            pane.hangup();
        }
        let Some(tab_index) = self.tabs.iter().position(|t| t.root.contains(id)) else { return };
        let tab = &mut self.tabs[tab_index];
        if !tab.root.remove(id) {
            self.tabs.remove(tab_index);
            self.tab_menu = None;
            self.renaming = None;
            if !self.tabs.is_empty() {
                self.save_tabs();
            }
            if self.tabs.is_empty() {
                self.quit();
                return;
            }
            self.switch_tab(if tab_index < self.active || self.active >= self.tabs.len() {
                self.active.saturating_sub(1).min(self.tabs.len() - 1)
            } else {
                self.active
            });
            return;
        }
        tab.zoomed = false;
        if tab.focus == id {
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            tab.focus = leaves[0];
        }
        self.relayout();
        self.save_tabs();
    }

    fn close_tab(&mut self, index: usize) {
        let mut leaves = Vec::new();
        self.tabs[index].root.leaves(&mut leaves);
        for id in leaves {
            self.close_pane(id);
        }
    }

    fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let before = self.focused_id();
        self.active = index;
        self.relayout();
        self.focus_moved(before);
    }

    fn set_focus(&mut self, id: u64) {
        let before = self.focused_id();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus = id;
        }
        self.relayout();
        self.focus_moved(before);
    }

    /// Tells programs that asked (focus reporting) that focus moved.
    fn focus_moved(&mut self, before: Option<u64>) {
        let after = self.focused_id();
        if before == after {
            return;
        }
        if before.is_some() {
            self.last_focus = before;
        }
        let now = Instant::now();
        self.ctx.blink_epoch = now;
        let window = self.focused;
        let idle = recap_idle();
        if let Some(pane) = before.and_then(|id| self.panes.iter_mut().find(|p| p.id == id)) {
            pane.focus_changed(false);
            pane.focus_lost(now);
        }
        let note = after.and_then(|id| self.mark_note(id, now));
        if let Some(pane) = after.and_then(|id| self.panes.iter_mut().find(|p| p.id == id)) {
            pane.focus_changed(window);
            if window {
                pane.focus_gained(now, Some(idle), note);
            }
        }
    }

    /// The panes of the active tab in reading order, for Command+[ and ].
    fn cycle_focus(&mut self, delta: i32) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let mut leaves = Vec::new();
        tab.root.leaves(&mut leaves);
        let at = leaves.iter().position(|&id| id == tab.focus).unwrap_or(0) as i32;
        let next = leaves[(at + delta).rem_euclid(leaves.len() as i32) as usize];
        self.set_focus(next);
    }

    /// Command+`: the pane whose agent has waited on you longest, else the
    /// one longest done with a turn you haven't seen; with Shift, the
    /// newest. With nothing to see, back to the pane you were in before.
    fn jump_to_needs(&mut self, newest: bool) {
        let focus = self.focused_id();
        let mut needs: Vec<(AgentState, Instant, u64)> = self
            .panes
            .iter()
            .filter(|pane| Some(pane.id) != focus)
            // Tabs set aside by hand wait their turn.
            .filter(|pane| !self.mark_of(pane.id).is_some_and(Mark::aside))
            .filter_map(|pane| pane.needs_since.map(|(state, at)| (state, at, pane.id)))
            .collect();
        let important = |id: u64| self.mark_of(id) == Some(Mark::Important);
        needs.sort_by(|a, b| {
            important(b.2).cmp(&important(a.2)).then(b.0.cmp(&a.0)).then(if newest {
                b.1.cmp(&a.1)
            } else {
                a.1.cmp(&b.1)
            })
        });
        let back = self.last_focus.filter(|id| Some(*id) != focus && self.panes.iter().any(|pane| pane.id == *id));
        match needs.first().map(|need| need.2).or(back) {
            Some(id) => self.reveal_pane(id),
            None => self.ctx.show_toast("Nothing needs you"),
        }
    }

    /// Focuses pane `id`, in whichever tab it's in.
    fn reveal_pane(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| {
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            leaves.contains(&id)
        }) else {
            return;
        };
        let before = self.focused_id();
        self.active = index;
        self.tabs[index].focus = id;
        self.relayout();
        self.focus_moved(before);
    }

    /// Moves focus to the nearest pane in a direction (Command+Option+arrow).
    fn focus_toward(&mut self, dx: f32, dy: f32) {
        let Some(focus) = self.focused_id() else { return };
        let Some(from) = self.panes.iter().find(|p| p.id == focus).map(|p| p.rect) else { return };
        let (cx, cy) = (from.x + from.w / 2.0, from.y + from.h / 2.0);
        let best = self
            .visible_panes()
            .into_iter()
            .filter(|(id, _)| *id != focus)
            .filter_map(|(id, r)| {
                let (px, py) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                let along = (px - cx) * dx + (py - cy) * dy;
                let across = ((px - cx) * dy).abs() + ((py - cy) * dx).abs();
                (along > 0.0).then_some((id, along + 2.0 * across))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((id, _)) = best {
            self.set_focus(id);
        }
    }

    fn toggle_zoom(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            tab.zoomed = !tab.zoomed && leaves.len() > 1;
            self.relayout();
        }
    }

    fn tab_list_visible(&self) -> bool {
        self.tab_list.unwrap_or(self.tabs.len() > 1)
    }

    /// How much of the window's top the title bar takes, in zoomed pixels.
    fn top(&self) -> f32 {
        platform::TITLEBAR / self.zoom
    }

    /// The tab list's area, under the title bar; empty while it is hidden.
    fn tab_area(&self) -> Rect {
        if !self.tab_list_visible() {
            return Rect::default();
        }
        let (top, height) = (self.top(), (self.height - self.top()).max(1.0));
        match self.strip {
            TabStrip::Left if self.compact => Rect::new(0.0, top, SIDEBAR_COMPACT, height),
            TabStrip::Left => Rect::new(0.0, top, self.sidebar_width.min(self.width / 2.0), height),
            TabStrip::Top => Rect::new(0.0, top, self.width, TAB_BAR_HEIGHT),
        }
    }

    /// Where a press grabs the sidebar's edge; empty without a sidebar.
    fn sidebar_edge(&self) -> Rect {
        let area = self.tab_area();
        if self.strip != TabStrip::Left || area.w == 0.0 {
            return Rect::default();
        }
        Rect::new(area.right() - GRAB, area.y, 2.0 * GRAB, area.h)
    }

    fn set_compact(&mut self, compact: bool) {
        self.compact = compact;
        self.relayout();
        self.save_sidebar();
    }

    fn set_extended(&mut self, extended: bool) {
        self.extended = extended;
        if extended && self.compact {
            self.compact = false;
            self.relayout();
        }
        if extended && !self.tab_list_visible() {
            self.tab_list = Some(true);
            self.relayout();
        }
        self.dirty = true;
        self.save_sidebar();
    }

    fn save_sidebar(&self) {
        let mode = match (self.compact, self.extended) {
            (true, _) => "compact",
            (false, true) => "extended",
            (false, false) => "full",
        };
        crate::save_setting("sidebar", mode);
        crate::save_setting("sidebar_width", &format!("{}", self.sidebar_width.round()));
    }

    fn content_rect(&self) -> Rect {
        let (width, top) = (self.width, self.top());
        let height = (self.height - top).max(1.0);
        let area = self.tab_area();
        match self.strip {
            TabStrip::Left => Rect::new(area.w, top, (width - area.w).max(1.0), height),
            TabStrip::Top => Rect::new(0.0, top + area.h, width, (height - area.h).max(1.0)),
        }
    }

    /// Lays out one tab: its panes' rectangles and its dividers.
    fn tab_layout(&self, index: usize) -> (Vec<(u64, Rect)>, Vec<Divider>) {
        let tab = &self.tabs[index];
        let rect = self.content_rect();
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        if tab.zoomed {
            panes.push((tab.focus, rect));
        } else {
            tab.root.layout(rect, &mut Vec::new(), &mut panes, &mut dividers);
        }
        // A pane with an agent (or a listed program) in it gives its top
        // to the header.
        for (id, rect) in &mut panes {
            let header = self.headers
                && self
                    .panes
                    .iter()
                    .any(|pane| pane.id == *id && (pane.agent_now.is_some() || pane.program_now.is_some()));
            if header && rect.h > 3.0 * AGENT_HEADER {
                rect.y += AGENT_HEADER;
                rect.h -= AGENT_HEADER;
            }
        }
        (panes, dividers)
    }

    fn visible_panes(&self) -> Vec<(u64, Rect)> {
        if self.tabs.is_empty() {
            return Vec::new();
        }
        self.tab_layout(self.active).0
    }

    /// Gives every pane (in every tab) its place, so switching tabs never
    /// has to rewrap. Zoomed-away panes keep their size.
    fn relayout(&mut self) {
        for index in 0..self.tabs.len() {
            let (rects, dividers) = self.tab_layout(index);
            for (id, rect) in rects {
                let text = &self.ctx.text;
                if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
                    pane.set_rect(rect, text);
                }
            }
            if index == self.active {
                self.dividers = dividers;
            }
        }
        self.dirty = true;
    }

    // ---- the loop -----------------------------------------------------

    fn handle(&mut self, message: Msg) {
        match message {
            Msg::Pty(id, bytes) => {
                let Self { panes, ctx, .. } = self;
                let mut moved = false;
                if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                    let before = pane.session.cwd.clone();
                    pane.feed(ctx, &bytes);
                    moved = pane.session.cwd != before;
                }
                // Each `cd` is written down at once, so a crash, force quit
                // or logout still reopens the panes where they were.
                if moved {
                    self.save_tabs();
                }
                self.check_install(id);
                self.dirty = true;
            }
            Msg::ShellExited(id, code) => {
                // With other panes around, `exit` closes this one; the last
                // one offers a new shell instead.
                if self.panes.len() > 1 {
                    self.close_pane(id);
                } else if let Some(pane) = self.pane(id) {
                    pane.session.shell_exited(code);
                }
                self.dirty = true;
            }
            Msg::Ui(event) => self.ui_event(event),
            Msg::Git(id, cwd, changed) => {
                if let Some(pane) = self.pane(id) {
                    pane.git_changes = Some((cwd, changed));
                }
                self.dirty = true;
            }
            Msg::Notice(message) => {
                self.ctx.toast = Some((message, Instant::now() + Duration::from_secs(6)));
                self.dirty = true;
            }
            Msg::Update(event) => {
                match event {
                    update::Event::Available(release) => {
                        self.update = Some(UpdateOffer { release, open: true, installing: false })
                    }
                    update::Event::Installed => {
                        self.relaunch = true;
                        self.quit();
                    }
                    update::Event::Failed(error) => {
                        if let Some(offer) = &mut self.update {
                            offer.installing = false;
                        }
                        self.ctx.toast =
                            Some((format!("Update failed: {error}"), Instant::now() + Duration::from_secs(6)));
                    }
                }
                self.dirty = true;
            }
        }
        self.update_title();
    }

    fn tick(&mut self) {
        let now = Instant::now();
        for pane in &mut self.panes {
            if pane.tick(now) {
                self.dirty = true;
            }
        }
        if self.ctx.toast.as_ref().is_some_and(|(_, at)| now >= *at) {
            self.ctx.toast = None;
            self.dirty = true;
        }
        if self.animation_at.is_some_and(|at| now >= at) {
            self.dirty = true;
        }
        if self.recap_test.is_some_and(|at| now >= at) {
            self.recap_test = None;
            if let Some(id) = self.focused_id() {
                let note = self.mark_note(id, now);
                if let Some(pane) = self.pane(id) {
                    pane.focus_gained(now, None, note);
                }
            }
            self.dirty = true;
        }
    }

    fn next_wakeup(&self) -> Option<Instant> {
        let mut times: Vec<Option<Instant>> = self.panes.iter().map(Pane::next_wakeup).collect();
        times.push(self.animation_at);
        times.push(self.recap_test);
        times.push(self.ctx.toast.as_ref().map(|(_, at)| *at));
        times.into_iter().flatten().min()
    }

    fn update_title(&mut self) {
        let Some(id) = self.focused_id() else { return };
        let title = match self.panes.iter().find(|p| p.id == id) {
            Some(pane) => format!("{} — Temino", pane.title()),
            None if id == SETTINGS => "Settings — Temino".to_owned(),
            None => return,
        };
        if title != self.title {
            platform::set_title(&title);
            self.title = title;
        }
    }

    fn set_cursor(&mut self, cursor: Cursor) {
        if cursor != self.cursor {
            platform::set_cursor(cursor);
            self.cursor = cursor;
        }
    }

    fn quit(&mut self) {
        self.save_tabs();
        for pane in &mut self.panes {
            pane.hangup();
        }
        update::finish(self.relaunch);
        self.quitting = true;
        platform::quit();
    }

    // ---- events -------------------------------------------------------

    fn ui_event(&mut self, event: UiEvent) {
        self.dirty = true;
        match event {
            UiEvent::Key(key) => {
                self.ctx.blink_epoch = Instant::now();
                self.key(key);
            }
            UiEvent::Resize { width, height, scale } => {
                self.window = (width as f32, height as f32);
                self.scale = scale;
                self.set_zoom(self.zoom, false);
            }
            UiEvent::Focus(focused) => {
                let now = Instant::now();
                self.focused = focused;
                self.ctx.blink_epoch = now;
                if let Some(id) = self.focused_id() {
                    let note = self.mark_note(id, now);
                    if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
                        pane.focus_changed(focused);
                        // Back from another app counts as coming back to the pane.
                        if focused {
                            pane.focus_gained(now, Some(recap_idle()), note);
                        } else {
                            pane.focus_lost(now);
                        }
                    }
                }
                self.dirty = true;
            }
            UiEvent::Close => self.quit(),
            UiEvent::Text(text) => {
                self.preedit.clear();
                self.ctx.blink_epoch = Instant::now();
                if let Some((_, field, selected)) = self.field.as_mut() {
                    if std::mem::take(selected) {
                        field.clear();
                    }
                    field.extend(text.chars().filter(|c| !c.is_control()));
                    self.dirty = true;
                    return;
                }
                if let Some(id) = self.focused_id() {
                    if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
                        pane.text_input(&text);
                    }
                }
            }
            UiEvent::Preedit(text) => self.preedit = text,
            UiEvent::Background(index) => self.set_background(index),
            UiEvent::Shell(index) => self.set_shell(index),
            UiEvent::Wheel { dy, x, y, precise, .. } => {
                let (x, y) = (x / self.zoom, y / self.zoom);
                if let Some((SETTINGS, _)) = self.pane_at(x, y) {
                    self.settings.wheel(dy);
                } else if let Some((id, rect)) = self.pane_at(x, y) {
                    let Self { panes, ctx, .. } = self;
                    if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                        pane.wheel(ctx, dy, precise, x - rect.x, y - rect.y);
                    }
                }
            }
            UiEvent::MouseDown { x, y, buttons, clicks, mods } => {
                self.mouse_down(x / self.zoom, y / self.zoom, buttons, clicks, mods)
            }
            UiEvent::MouseMoved { x, y, buttons } => self.mouse_moved(x / self.zoom, y / self.zoom, buttons),
            UiEvent::MouseUp { x, y } => self.mouse_up(x / self.zoom, y / self.zoom),
        }
    }

    fn set_background(&mut self, index: usize) {
        let Some(&(name, rgb)) = BACKGROUNDS.get(index) else { return };
        self.ctx.theme = Theme::dark().with_background(rgb).with_opacity(self.ctx.theme.opacity);
        platform::set_background(self.ctx.theme.background.to_rgb8());
        platform::set_background_menu(&BACKGROUNDS.map(|(name, _)| name), index);
        self.background = index;
        crate::save_setting("background", name);
        self.dirty = true;
    }

    fn set_opacity(&mut self, index: usize) {
        let Some(&percent) = OPACITIES.get(index) else { return };
        self.ctx.theme.opacity = percent as f32 / 100.0;
        self.opacity = index;
        crate::save_setting("opacity", &percent.to_string());
        self.apply_blur();
        self.dirty = true;
    }

    /// The window blurs what's behind it only while it is see-through.
    fn apply_blur(&self) {
        platform::set_blur(self.blur && self.ctx.theme.opacity < 1.0);
    }

    /// Settings ▸ Shell: new panes start this shell; a missing one is
    /// installed first, in a new tab, and picked once that succeeds.
    fn set_shell(&mut self, index: usize) {
        let Some(&kind) = KINDS.get(index) else { return };
        if let Some(path) = shells::find(kind) {
            self.use_shell(path);
            return;
        }
        match shells::install_command(kind) {
            Ok(command) => match self.new_tab() {
                Ok(()) => {
                    if let Some(id) = self.focused_id() {
                        if let Some(pane) = self.pane(id) {
                            pane.run_when_ready(command);
                        }
                        self.installing = Some((id, kind));
                    }
                }
                Err(error) => self.ctx.show_toast(&error),
            },
            Err(why) => self.ctx.show_toast(&why),
        }
        self.refresh_shell_menu();
    }

    fn use_shell(&mut self, path: String) {
        let Some(kind) = shells::Kind::of_path(&path) else { return };
        crate::save_setting("shell", kind.name());
        self.ctx.shell = path;
        self.refresh_shell_menu();
        self.ctx.show_toast(&format!("New tabs and panes open {}", kind.name()));
        self.dirty = true;
    }

    fn refresh_shell_menu(&self) {
        let (names, selected) = shells::menu(&self.ctx.shell);
        platform::set_shell_menu(&names, selected);
    }

    /// After a Settings ▸ Shell install command finishes: use the shell if
    /// it is there now.
    fn check_install(&mut self, id: u64) {
        let Some((pane_id, kind)) = self.installing else { return };
        if pane_id != id {
            return;
        }
        let Some(code) = self.pane(id).and_then(|pane| pane.take_queued_exit()) else { return };
        self.installing = None;
        match shells::find(kind) {
            Some(path) => self.use_shell(path),
            None => {
                self.refresh_shell_menu();
                self.ctx.show_toast(&format!("Installing {} failed (status {code})", kind.name()));
            }
        }
    }

    /// Zooms the whole window, text and chrome alike, so the terminal font
    /// is `size` points.
    fn change_font_size(&mut self, size: f32) {
        self.set_zoom(size / BASE_FONT_SIZE, true);
    }

    /// Steps the zoom 10% of the default size up or down, onto that grid,
    /// so a step each way comes back to where it started.
    fn step_font_size(&mut self, step: f32) {
        let tenths = (self.zoom_percent() / 10.0 + step).round();
        self.change_font_size(DEFAULT_FONT_SIZE * tenths / 10.0);
    }

    /// The zoom as Settings and the toast show it: 100% at the default size.
    fn zoom_percent(&self) -> f32 {
        self.zoom * BASE_FONT_SIZE / DEFAULT_FONT_SIZE * 100.0
    }

    /// Lays everything out at `zoom`: a smaller logical window drawn at a
    /// larger scale, so every size in the UI grows together.
    fn set_zoom(&mut self, zoom: f32, announce: bool) {
        let zoom = zoom.clamp(8.0 / BASE_FONT_SIZE, 32.0 / BASE_FONT_SIZE);
        let changed = zoom != self.zoom;
        self.zoom = zoom;
        self.width = self.window.0 / zoom;
        self.height = self.window.1 / zoom;
        if self.ctx.text.scale != self.scale * zoom {
            self.ctx.text.set_scale(self.scale * zoom);
            for pane in &mut self.panes {
                pane.cells_changed(&self.ctx.text);
            }
        }
        self.relayout();
        if changed && announce {
            crate::save_setting("zoom", &format!("{zoom}"));
            self.ctx.show_toast(&format!("Zoom {:.0}%", self.zoom_percent()));
        }
    }

    /// Window shortcuts (tabs, splits, font size); the rest go to the pane.
    fn key(&mut self, key: KeyEvent) {
        if self.tab_menu.take().is_some() && key.named == Some(Named::Escape) {
            return;
        }
        if key.named == Some(Named::Escape) {
            if let Some(offer) = self.update.as_mut().filter(|offer| offer.open) {
                offer.open = false;
                return;
            }
        }
        if self.rename_key(&key) || self.field_key(&key) {
            return;
        }
        let named = key.named;
        let several_tabs = self.tabs.len() > 1;
        if key.ctrl() && !key.cmd() {
            let handled = match named {
                Some(Named::Tab) => {
                    self.step_tab(if key.shift() { -1 } else { 1 });
                    true
                }
                Some(Named::PageUp) if several_tabs && !key.shift() => {
                    self.step_tab(-1);
                    true
                }
                Some(Named::PageDown) if several_tabs && !key.shift() => {
                    self.step_tab(1);
                    true
                }
                _ => false,
            };
            if handled {
                return;
            }
        }
        if key.cmd() && !key.ctrl() {
            let handled = match (named, key.base_char()) {
                (Some(Named::Left), _) if key.option() => {
                    self.focus_toward(-1.0, 0.0);
                    true
                }
                (Some(Named::Right), _) if key.option() => {
                    self.focus_toward(1.0, 0.0);
                    true
                }
                (Some(Named::Up), _) if key.option() => {
                    self.focus_toward(0.0, -1.0);
                    true
                }
                (Some(Named::Down), _) if key.option() => {
                    self.focus_toward(0.0, 1.0);
                    true
                }
                (Some(Named::Enter), _) => {
                    self.toggle_zoom();
                    true
                }
                (Some(Named::PageUp), _) => {
                    self.step_tab(-1);
                    true
                }
                (Some(Named::PageDown), _) => {
                    self.step_tab(1);
                    true
                }
                (_, Some('t')) => {
                    if let Err(error) = self.new_tab() {
                        self.ctx.show_toast(&error);
                    }
                    true
                }
                (_, Some('i')) if key.shift() => {
                    self.toggle_mark(self.active, Mark::Important);
                    true
                }
                (_, Some('l')) if key.shift() => {
                    self.toggle_mark(self.active, Mark::Later);
                    true
                }
                (_, Some('l')) => {
                    self.toggle_mark(self.active, Mark::Waiting);
                    true
                }
                (_, Some('i')) => {
                    // "Where was I?" on demand.
                    if let Some(id) = self.focused_id() {
                        let now = Instant::now();
                        let note = self.mark_note(id, now);
                        if let Some(pane) = self.pane(id) {
                            pane.focus_gained(now, None, note);
                        }
                    }
                    true
                }
                (_, Some('b')) if key.shift() => {
                    self.set_extended(!self.extended);
                    true
                }
                (_, Some('b')) => {
                    self.toggle_tab_list();
                    true
                }
                (_, Some('d')) => {
                    self.split(Axis::Row);
                    true
                }
                (_, Some('e')) => {
                    self.split(Axis::Column);
                    true
                }
                (_, Some('w')) => {
                    if let Some(id) = self.focused_id() {
                        self.close_pane(id);
                    }
                    true
                }
                (_, Some('q')) => {
                    self.quit();
                    true
                }
                (_, Some(',')) => {
                    self.open_settings();
                    true
                }
                (_, Some('u')) => {
                    self.check_for_updates();
                    true
                }
                // Command+Shift+[ ] (macOS-style) moves between tabs.
                (_, Some('[' | '{')) if key.shift() => {
                    self.step_tab(-1);
                    true
                }
                (_, Some(']' | '}')) if key.shift() => {
                    self.step_tab(1);
                    true
                }
                (_, Some('[')) => {
                    self.cycle_focus(-1);
                    true
                }
                (_, Some(']')) => {
                    self.cycle_focus(1);
                    true
                }
                (_, Some('`' | '~')) => {
                    self.jump_to_needs(key.shift());
                    true
                }
                (_, Some(c @ '1'..='9')) => {
                    let n = c as usize - '1' as usize;
                    self.switch_tab(if c == '9' { self.tabs.len() - 1 } else { n });
                    true
                }
                (_, Some('=' | '+')) => {
                    self.step_font_size(1.0);
                    true
                }
                (_, Some('-')) => {
                    self.step_font_size(-1.0);
                    true
                }
                (_, Some('0')) => {
                    self.change_font_size(DEFAULT_FONT_SIZE);
                    true
                }
                _ => false,
            };
            if handled {
                return;
            }
        }
        let Some(id) = self.focused_id() else { return };
        // Typing in a tab set aside means it's being dealt with.
        if let Some(tab) = self.tabs.get_mut(self.active).filter(|tab| tab.mark.is_some_and(Mark::aside)) {
            tab.mark = None;
            self.save_tabs();
        }
        let Self { panes, ctx, .. } = self;
        if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
            pane.key(ctx, key);
        }
    }

    /// Command+,: the Settings tab, opened after the others or switched to.
    fn open_settings(&mut self) {
        self.installed = KINDS.map(|kind| shells::find(kind).is_some());
        if let Some(index) = self.tabs.iter().position(|tab| tab.focus == SETTINGS) {
            self.switch_tab(index);
            return;
        }
        self.tabs.push(Tab {
            root: Node::Leaf(SETTINGS),
            focus: SETTINGS,
            zoomed: false,
            name: None,
            color: None,
            shown: None,
            slide: None,
            mark: None,
            marked_at: None,
        });
        self.switch_tab(self.tabs.len() - 1);
    }

    fn settings_values(&self) -> settings::Values {
        settings::Values {
            background: self.background,
            opacity: self.opacity,
            blur: self.blur,
            shell: shells::Kind::of_path(&self.ctx.shell).and_then(|kind| KINDS.iter().position(|&k| k == kind)),
            installed: self.installed,
            tabs_top: self.strip == TabStrip::Top,
            zoom: self.zoom_percent(),
            updates: crate::setting("updates").as_deref() != Some("off"),
            see_through: cfg!(target_os = "macos"),
            fields: Field::ALL.map(|field| crate::setting(field.key()).unwrap_or_default()),
            editing: self.field.clone(),
            headers: self.headers,
        }
    }

    fn settings_action(&mut self, action: Action) {
        self.dirty = true;
        match action {
            Action::Background(index) => self.set_background(index),
            Action::Opacity(index) => self.set_opacity(index),
            Action::Blur(on) => {
                self.blur = on;
                crate::save_setting("blur", if on { "on" } else { "off" });
                self.apply_blur();
            }
            Action::Shell(index) => {
                self.set_shell(index);
                self.installed = KINDS.map(|kind| shells::find(kind).is_some());
            }
            Action::TabsTop(top) => {
                self.strip = if top { TabStrip::Top } else { TabStrip::Left };
                crate::save_setting("tabs", if top { "top" } else { "left" });
                self.relayout();
            }
            Action::Zoom(0) => self.change_font_size(DEFAULT_FONT_SIZE),
            Action::Zoom(step) => self.step_font_size(step as f32),
            Action::Updates(on) => crate::save_setting("updates", if on { "on" } else { "off" }),
            Action::CheckForUpdates => {
                self.check_for_updates();
            }
            Action::Headers(on) => {
                self.headers = on;
                crate::save_setting("headers", if on { "on" } else { "off" });
                self.relayout();
            }
            Action::Edit(field) => {
                self.finish_field();
                let text = crate::setting(field.key()).unwrap_or_default();
                self.field = Some((field, text, true));
            }
        }
    }

    /// Ends typing in a Settings field, keeping what was typed.
    fn finish_field(&mut self) {
        let Some((field, text, _)) = self.field.take() else { return };
        crate::save_setting(field.key(), text.trim());
        if matches!(field, Field::LlmUrl | Field::LlmKey) {
            crate::llm::forget_model();
        }
        self.dirty = true;
    }

    /// A key while typing in a Settings field; false lets it through.
    fn field_key(&mut self, key: &KeyEvent) -> bool {
        let Some((_, text, selected)) = self.field.as_mut() else { return false };
        self.dirty = true;
        if key.cmd() {
            match key.base_char() {
                Some('a') => *selected = true,
                Some('v') => {
                    // Keys come pasted: the first line, without spaces round it.
                    let pasted = platform::clipboard_get().unwrap_or_default();
                    let pasted = pasted.lines().next().unwrap_or("").trim();
                    if *selected {
                        text.clear();
                    }
                    text.push_str(pasted);
                    *selected = false;
                }
                _ => {
                    self.finish_field();
                    return false;
                }
            }
            return true;
        }
        match key.named {
            Some(Named::Enter | Named::Tab) => self.finish_field(),
            Some(Named::Escape) => self.field = None,
            Some(Named::Backspace | Named::Delete) if *selected => text.clear(),
            Some(Named::Backspace) => {
                text.pop();
            }
            Some(_) => {}
            None if !key.ctrl() => {
                let typed: String = String::from_utf8_lossy(&key.bytes).chars().filter(|c| !c.is_control()).collect();
                if typed.is_empty() {
                    return true;
                }
                if *selected {
                    text.clear();
                }
                text.push_str(&typed);
            }
            None => {}
        }
        if let Some((_, _, selected)) = self.field.as_mut() {
            *selected = false;
        }
        true
    }

    /// Command+U, the menu and Settings: the card again if an update is
    /// already offered, else a fresh check.
    fn check_for_updates(&mut self) {
        if let Some(offer) = &mut self.update {
            offer.open = true;
            return;
        }
        self.ctx.show_toast("Checking for updates…");
        update::check(self.ctx.sender.clone(), true);
    }

    fn update_action(&mut self, action: UpdateAction) {
        let Some(offer) = &mut self.update else { return };
        match action {
            UpdateAction::Toggle => offer.open = !offer.open,
            UpdateAction::Notes => platform::open_url(&offer.release.notes_url()),
            // The download can't be called back once started.
            _ if offer.installing => {}
            UpdateAction::Later => offer.open = false,
            UpdateAction::Skip => {
                update::skip(&offer.release.version);
                self.update = None;
            }
            UpdateAction::Install => {
                offer.installing = true;
                update::install(self.ctx.sender.clone(), offer.release.clone());
            }
        }
    }

    fn toggle_tab_list(&mut self) {
        let visible = !self.tab_list_visible();
        self.tab_list = Some(visible);
        self.relayout();
        if !visible {
            self.ctx.show_toast(&format!("Tabs hidden · {}+B shows them", platform::COMMAND_NAME));
        }
    }

    fn step_tab(&mut self, delta: i32) {
        let n = self.tabs.len() as i32;
        if n > 1 {
            self.switch_tab((self.active as i32 + delta).rem_euclid(n) as usize);
        }
    }

    /// Moves tab `from` to position `to` in the list; the active tab stays active.
    /// Sets tab `index`'s mark to `mark`, or clears it when it's already set.
    fn toggle_mark(&mut self, index: usize, mark: Mark) {
        let Some(tab) = self.tabs.get_mut(index) else { return };
        tab.mark = if tab.mark == Some(mark) { None } else { Some(mark) };
        tab.marked_at = tab.mark.map(|_| Instant::now());
        let toast = match tab.mark {
            Some(mark) => format!("Marked {}", mark.label()),
            None => "Mark cleared".to_owned(),
        };
        self.ctx.show_toast(&toast);
        self.save_tabs();
        self.dirty = true;
    }

    /// The tab holding pane `id`.
    fn tab_of(&self, id: u64) -> Option<&Tab> {
        self.tabs.iter().find(|tab| {
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            leaves.contains(&id)
        })
    }

    /// The mark on the tab holding pane `id`.
    fn mark_of(&self, id: u64) -> Option<Mark> {
        self.tab_of(id).and_then(|tab| tab.mark)
    }

    /// What the mark on pane `id`'s tab adds to its recap.
    fn mark_note(&self, id: u64, now: Instant) -> Option<MarkNote> {
        let tab = self.tab_of(id)?;
        let mark = tab.mark?;
        let ago = tab.marked_at.map(|at| ui::short_duration(now.duration_since(at)));
        Some(MarkNote {
            fact: match &ago {
                Some(ago) => format!("{} · marked {ago} ago", mark.label()),
                None => mark.label().to_owned(),
            },
            context: match &ago {
                Some(ago) => format!("The user marked this tab {} {ago} ago.", mark.meaning()),
                None => format!("The user marked this tab {}.", mark.meaning()),
            },
        })
    }

    fn move_tab(&mut self, from: usize, to: usize) {
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        // Every tab between the two ends moved; each slides over from where
        // it shows now.
        let now = Instant::now();
        for tab in &mut self.tabs[from.min(to)..=from.max(to)] {
            tab.slide = tab.shown.map(|at| (at, now));
        }
        self.active = if self.active == from {
            to
        } else if from < self.active && self.active <= to {
            self.active - 1
        } else if to <= self.active && self.active < from {
            self.active + 1
        } else {
            self.active
        };
    }

    fn start_rename(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else { return };
        let name = tab.name.clone().unwrap_or_else(|| self.tab_title(index));
        self.renaming = Some(Rename { tab: index, name, selected: true });
    }

    /// Ends renaming, keeping the name typed; an empty name goes back to the
    /// pane's title.
    fn finish_rename(&mut self) {
        let Some(rename) = self.renaming.take() else { return };
        if let Some(tab) = self.tabs.get_mut(rename.tab) {
            let name = rename.name.trim().to_owned();
            // Left as it was, the title keeps following the panes.
            let typed = !name.is_empty() && tab.name.as_deref() != Some(&name);
            if typed || name.is_empty() {
                let auto = self.auto_title(rename.tab);
                let tab = &mut self.tabs[rename.tab];
                tab.name = (typed && name != auto).then_some(name);
            }
        }
        self.save_tabs();
    }

    /// A key while renaming a tab; false lets it through.
    fn rename_key(&mut self, key: &KeyEvent) -> bool {
        let Some(rename) = self.renaming.as_mut() else { return false };
        if key.cmd() {
            if key.base_char() == Some('a') {
                rename.selected = true;
                return true;
            }
            self.finish_rename();
            return false;
        }
        match key.named {
            Some(Named::Enter) => self.finish_rename(),
            Some(Named::Escape) => self.renaming = None,
            Some(Named::Backspace | Named::Delete) if rename.selected => rename.name.clear(),
            Some(Named::Backspace) => {
                rename.name.pop();
            }
            Some(_) => {}
            None if !key.ctrl() => {
                let typed: String = String::from_utf8_lossy(&key.bytes).chars().filter(|c| !c.is_control()).collect();
                if typed.is_empty() {
                    return true;
                }
                if rename.selected {
                    rename.name.clear();
                }
                rename.name.push_str(&typed);
            }
            None => {}
        }
        if let Some(rename) = self.renaming.as_mut() {
            rename.selected = false;
        }
        true
    }

    fn open_tab_menu(&mut self, tab: usize, x: f32, y: f32) {
        self.finish_rename();
        self.switch_tab(tab);
        self.tab_menu = Some(TabMenu { tab, x, y, pane: false });
    }

    fn menu_action(&mut self, tab: usize, action: MenuAction) {
        self.tab_menu = None;
        match action {
            MenuAction::Rename => self.start_rename(tab),
            MenuAction::Color(color) => {
                if let Some(tab) = self.tabs.get_mut(tab) {
                    tab.color = color;
                }
                self.save_tabs();
            }
            MenuAction::Mark(mark) => self.toggle_mark(tab, mark),
            MenuAction::Close => self.close_tab(tab),
            MenuAction::CloseOthers => {
                for index in (0..self.tabs.len()).rev().filter(|&i| i != tab) {
                    self.close_tab(index);
                }
            }
            MenuAction::SplitRight => self.split(Axis::Row),
            MenuAction::SplitDown => self.split(Axis::Column),
            MenuAction::ClosePane => {
                if let Some(id) = self.focused_id() {
                    self.close_pane(id);
                }
            }
            MenuAction::Copy | MenuAction::Paste | MenuAction::Clear => {
                let Some(id) = self.focused_id() else { return };
                let Self { panes, ctx, .. } = self;
                let Some(pane) = panes.iter_mut().find(|p| p.id == id) else { return };
                match action {
                    MenuAction::Copy => pane.copy_selection(ctx),
                    MenuAction::Paste => pane.paste_clipboard(),
                    _ => pane.clear(),
                }
            }
        }
    }

    /// The tab whose slot in the list is under the pointer, along the list.
    fn tab_slot_at(&self, x: f32, y: f32) -> Option<usize> {
        self.tab_hits
            .iter()
            .find(|(r, _)| match self.strip {
                TabStrip::Left => y >= r.y && y < r.bottom() + 4.0,
                TabStrip::Top => x >= r.x && x < r.right() + 4.0,
            })
            .map(|&(_, index)| index)
    }

    fn pane_at(&self, x: f32, y: f32) -> Option<(u64, Rect)> {
        self.visible_panes().into_iter().find(|(_, rect)| rect.contains(x, y))
    }

    fn mouse_down(&mut self, x: f32, y: f32, pressed: i32, clicks: i32, mods: u32) {
        // A click off the field being typed in ends it.
        let on_field = matches!(self.pane_at(x, y), Some((SETTINGS, _)))
            && matches!(self.settings.action_at(x, y), Some(Action::Edit(_)));
        if !on_field {
            self.finish_field();
        }
        if let Some(menu) = self.tab_menu.take() {
            // A click anywhere else only closes the menu.
            if let Some(&(_, action)) = self.menu_hits.iter().find(|(r, _)| r.contains(x, y)) {
                self.menu_action(menu.tab, action);
            } else if self.menu_rect.contains(x, y) {
                self.tab_menu = Some(menu);
            }
            return;
        }
        if let Some(&(_, action)) = self.update_hits.iter().find(|(r, _)| r.contains(x, y)) {
            self.update_action(action);
            return;
        }
        if self.update_card.contains(x, y) {
            return;
        }
        // A click elsewhere puts the card away, and goes on to what it hit.
        if let Some(offer) = self.update.as_mut().filter(|offer| offer.open && !offer.installing) {
            offer.open = false;
        }
        let on_renamed = self
            .renaming
            .as_ref()
            .is_some_and(|rename| self.tab_hits.iter().any(|(r, i)| *i == rename.tab && r.contains(x, y)));
        if !on_renamed {
            self.finish_rename();
        }
        if self.sidebar_edge().contains(x, y) {
            self.pointer = Pointer::Sidebar;
            return;
        }
        if let Some(&(_, tab, id)) = self.pane_hits.iter().find(|(r, _, _)| r.contains(x, y)) {
            // A pane's line in the extended sidebar, or its agent header:
            // go to that pane.
            if tab != self.active {
                self.switch_tab(tab);
            }
            if self.focused_id() != Some(id) {
                self.set_focus(id);
            }
            if self.tab_area().contains(x, y) {
                self.pointer = Pointer::Tab { index: tab, x, y, dragging: false };
            }
            return;
        }
        if self.tab_area().contains(x, y) {
            if self.collapse_hit.contains(x, y) {
                self.set_compact(!self.compact);
                return;
            }
            if self.extend_hit.contains(x, y) {
                self.set_extended(!self.extended);
                return;
            }
            // Control+click is the secondary click on macOS.
            let secondary =
                pressed & buttons::SECONDARY != 0 || (cfg!(target_os = "macos") && mods & platform::mods::CONTROL != 0);
            if let Some(&(_, index)) = self.close_hits.iter().find(|(r, _)| r.contains(x, y)) {
                self.close_tab(index);
            } else if let Some(&(r, index)) = self.more_hits.iter().find(|(r, _)| r.contains(x, y)) {
                self.open_tab_menu(index, r.x, r.bottom() + 4.0);
            } else if let Some(&(_, index)) = self.tab_hits.iter().find(|(r, _)| r.contains(x, y)) {
                if secondary {
                    self.open_tab_menu(index, x, y);
                } else if pressed & buttons::TERTIARY != 0 {
                    self.close_tab(index);
                } else if clicks == 2 && !on_renamed {
                    self.start_rename(index);
                } else if !on_renamed {
                    self.switch_tab(index);
                    self.pointer = Pointer::Tab { index, x, y, dragging: false };
                }
            } else if self.new_tab_hit.contains(x, y) {
                if let Err(error) = self.new_tab() {
                    self.ctx.show_toast(&error);
                }
            }
            return;
        }
        if let Some(divider) = self.dividers.iter().find(|d| d.grab_area().contains(x, y)) {
            self.pointer = Pointer::Divider(divider.clone());
            return;
        }
        let Some((id, rect)) = self.pane_at(x, y) else { return };
        if id == SETTINGS {
            if let Some(action) = self.settings.action_at(x, y) {
                self.settings_action(action);
            }
            return;
        }
        if self.focused_id() != Some(id) {
            self.set_focus(id);
        }
        // A right click (Control+click on macOS) opens the pane's menu,
        // unless the program takes the mouse itself.
        let secondary =
            pressed & buttons::SECONDARY != 0 || (cfg!(target_os = "macos") && mods & platform::mods::CONTROL != 0);
        let reports = self.panes.iter().find(|p| p.id == id).is_some_and(|p| p.reports_mouse());
        if secondary && !(reports && mods & platform::mods::SHIFT == 0) {
            self.finish_rename();
            self.tab_menu = Some(TabMenu { tab: self.active, x, y, pane: true });
            return;
        }
        self.pointer = Pointer::Pane(id);
        let Self { panes, ctx, .. } = self;
        if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
            pane.mouse_down(ctx, x - rect.x, y - rect.y, pressed, clicks, mods);
        }
    }

    fn mouse_moved(&mut self, x: f32, y: f32, pressed: i32) {
        self.mouse = (x, y);
        if !self.update_hits.is_empty() {
            // The pill and the card's buttons light up under the pointer.
            self.dirty = true;
            if self.update_card.contains(x, y) || self.update_hits.iter().any(|(r, _)| r.contains(x, y)) {
                self.set_cursor(Cursor::Arrow);
                return;
            }
        }
        if let Pointer::Tab { index, x: x0, y: y0, dragging } = self.pointer {
            if pressed == 0 {
                self.pointer = Pointer::Idle;
            } else {
                let dragging = dragging || (x - x0).abs().max((y - y0).abs()) > DRAG_SLOP;
                let mut index = index;
                if dragging {
                    if let Some(to) = self.tab_slot_at(x, y).filter(|&to| to != index) {
                        self.move_tab(index, to);
                        index = to;
                    }
                    self.set_cursor(Cursor::Arrow);
                }
                self.pointer = Pointer::Tab { index, x: x0, y: y0, dragging };
                return;
            }
        }
        let hover = self.tab_hits.iter().find(|(r, _)| r.contains(x, y)).map(|&(_, index)| index);
        if hover != self.hover_tab {
            self.dirty = true;
            self.hover_at = Instant::now();
        }
        self.hover_tab = hover;
        match &self.pointer {
            Pointer::Divider(divider) if pressed != 0 => {
                let divider = divider.clone();
                let parent = divider.parent;
                let ratio = match divider.axis {
                    Axis::Row => (x - parent.x) / parent.w,
                    Axis::Column => (y - parent.y) / parent.h,
                };
                let extent = if divider.axis == Axis::Row { parent.w } else { parent.h };
                let low = (MIN_PANE / extent).min(0.5);
                let ratio = ratio.clamp(low, 1.0 - low);
                let tab = &mut self.tabs[self.active];
                if let Some(Node::Split { ratio: r, .. }) = tab.root.at_path(&divider.path) {
                    *r = ratio;
                }
                self.relayout();
                return;
            }
            Pointer::Sidebar if pressed != 0 => {
                self.compact = x < SIDEBAR_SNAP;
                if !self.compact {
                    self.sidebar_width = x.clamp(SIDEBAR_MIN, SIDEBAR_MAX.min(self.width / 2.0));
                }
                self.set_cursor(Cursor::ResizeColumn);
                self.relayout();
                return;
            }
            Pointer::Pane(id) if pressed != 0 => {
                let id = *id;
                let Self { panes, ctx, .. } = self;
                if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                    let (ox, oy) = (pane.rect.x, pane.rect.y);
                    pane.mouse_moved(ctx, x - ox, y - oy, pressed);
                }
                return;
            }
            _ => {}
        }
        // Only the pane under the pointer underlines a link.
        let over_chrome = self.sidebar_edge().contains(x, y)
            || self.tab_area().contains(x, y)
            || self.dividers.iter().any(|d| d.grab_area().contains(x, y));
        let hovered = self.pane_at(x, y).map(|(id, _)| id).filter(|_| !over_chrome);
        for pane in self.panes.iter_mut().filter(|pane| Some(pane.id) != hovered) {
            pane.pointer_left();
        }
        if self.sidebar_edge().contains(x, y) {
            self.set_cursor(Cursor::ResizeColumn);
            return;
        }
        if self.tab_area().contains(x, y) {
            self.set_cursor(Cursor::Arrow);
            // The collapse button and compact rows' titles follow the pointer.
            self.dirty = true;
            return;
        }
        if let Some(divider) = self.dividers.iter().find(|d| d.grab_area().contains(x, y)) {
            let cursor = if divider.axis == Axis::Row { Cursor::ResizeColumn } else { Cursor::ResizeRow };
            self.set_cursor(cursor);
            return;
        }
        let Some((id, rect)) = self.pane_at(x, y) else { return };
        if id == SETTINGS {
            // Hover highlights follow the pointer.
            self.set_cursor(Cursor::Arrow);
            self.dirty = true;
            return;
        }
        let Self { panes, ctx, .. } = self;
        let cursor = panes.iter_mut().find(|p| p.id == id).map(|pane| pane.mouse_moved(ctx, x - rect.x, y - rect.y, 0));
        if let Some(cursor) = cursor {
            self.set_cursor(cursor);
        }
    }

    fn mouse_up(&mut self, x: f32, y: f32) {
        let pointer = std::mem::replace(&mut self.pointer, Pointer::Idle);
        if matches!(pointer, Pointer::Tab { dragging: true, .. } | Pointer::Divider(_)) {
            self.save_tabs();
        }
        if matches!(pointer, Pointer::Sidebar) {
            self.save_sidebar();
        }
        if let Pointer::Pane(id) = pointer {
            let Self { panes, ctx, .. } = self;
            if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                let (ox, oy) = (pane.rect.x, pane.rect.y);
                pane.mouse_up(ctx, x - ox, y - oy);
            }
        }
    }

    // ---- drawing ------------------------------------------------------

    /// The input method's composition, over the text at the cursor, underlined.
    fn draw_preedit(&mut self) {
        let Some(caret) = self.scene.caret else { return };
        if self.preedit.is_empty() {
            return;
        }
        let Self { scene, ctx, preedit, .. } = self;
        let cell = ctx.text.cell;
        let width = crate::editor::columns(preedit) as f32 * cell.width;
        scene.set_origin(0.0, 0.0);
        scene.set_layer(LAYER_OVERLAY);
        scene.fill(Rect::new(caret.x, caret.y, width, cell.height), ctx.theme.background);
        let mut x = caret.x;
        for c in preedit.chars() {
            let span = crate::editor::width(c);
            ctx.text.draw_cell(scene, c, x, caret.y, crate::text::Style::default(), ctx.theme.text, span.max(1));
            x += span as f32 * cell.width;
        }
        scene.fill(Rect::new(caret.x, caret.y + cell.height - 2.0, width, 1.0), ctx.theme.text);
        scene.set_layer(LAYER_CONTENT);
    }

    /// Tells the platform where the cursor is, so an input method's
    /// candidate window opens beside it.
    fn report_caret(&mut self) {
        let Some(caret) = self.scene.caret else { return };
        let zoom = self.zoom;
        let at = [(caret.x * zoom) as i32, (caret.y * zoom) as i32, (caret.h * zoom) as i32];
        if self.caret_sent != Some(at) {
            self.caret_sent = Some(at);
            platform::set_text_caret(at[0], at[1], at[2]);
        }
    }

    fn render(&mut self) {
        crate::trace("render: begin");
        self.dirty = false;
        let (width, height) = (self.width, self.height);
        self.scene.reset(width, height, self.scale * self.zoom, self.ctx.theme.window());
        self.ctx.composing = !self.preedit.is_empty();
        let agents = self.refresh_agents();
        let focus = self.focused_id();
        let visible = self.visible_panes();
        let several = visible.len() > 1;
        let mut animation: Option<Instant> = None;
        for &(id, rect) in &visible {
            if id == SETTINGS {
                let values = self.settings_values();
                let Self { settings, scene, ctx, mouse, .. } = self;
                settings.draw(scene, &mut ctx.text, &ctx.theme, rect, &values, *mouse);
            }
        }
        for (id, _) in &visible {
            let focused = Some(*id) == focus && self.focused;
            let Self { panes, ctx, scene, .. } = self;
            let Some(pane) = panes.iter_mut().find(|p| p.id == *id) else { continue };
            pane.draw(scene, ctx, focused);
            if let Some(at) = pane.animation_deadline(ctx, focused) {
                animation = Some(animation.map_or(at, |a| a.min(at)));
            }
            if several && Some(*id) != focus {
                // Inactive panes recede a little, as in Warp.
                scene.set_layer(LAYER_OVERLAY);
                scene.fill(pane.rect, ctx.theme.background.alpha(0.35));
                scene.set_layer(LAYER_CONTENT);
            }
        }
        self.animation_at = animation;
        self.wake_for_agents(&agents);
        self.pane_hits.clear();
        self.draw_agent_headers(&visible);
        for divider in &self.dividers {
            self.scene.set_layer(LAYER_OVERLAY);
            self.scene.fill(divider.line, self.ctx.theme.divider.mix(self.ctx.theme.text_faint, 0.35));
            self.scene.set_layer(LAYER_CONTENT);
        }
        self.draw_tab_bar(&agents);
        self.draw_titlebar();
        self.draw_update();
        self.draw_tab_menu();
        self.draw_toast();
        self.draw_preedit();
        self.report_caret();
        crate::trace("render: scene built");
        let capture = std::env::var_os("TEMINO_CAPTURE");
        let on_screen = self.gpu.on_screen();
        match self.gpu.render(&self.scene, &mut self.ctx.text.atlas, &mut self.ctx.text.color_atlas, capture.is_some())
        {
            Ok(Some(pixels)) => {
                let (pw, ph) = self.scene.pixel_size();
                if let Some(path) = &capture {
                    write_ppm(path, pixels, pw, ph);
                }
                if !on_screen {
                    platform::present(pixels, pw, ph)
                }
            }
            // Already in the window (or skipped: no drawable came free).
            Ok(None) => {}
            Err(error) => {
                crate::fatal(&format!("rendering failed: {error}"));
                self.quit();
            }
        }
    }

    /// A tab's name, or its focused pane's title.
    fn tab_title(&self, index: usize) -> String {
        let tab = &self.tabs[index];
        if tab.focus == SETTINGS {
            return "Settings".to_owned();
        }
        tab.name.clone().unwrap_or_else(|| self.auto_title(index))
    }

    /// A tab's title when it isn't named: its panes' folders, in layout
    /// order and each once, then what the focused pane has been running.
    fn auto_title(&self, index: usize) -> String {
        let tab = &self.tabs[index];
        let mut leaves = Vec::new();
        tab.root.leaves(&mut leaves);
        let mut folders: Vec<String> = Vec::new();
        for pane in leaves.iter().filter_map(|id| self.panes.iter().find(|p| p.id == *id)) {
            let folder = pane.folder();
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        let recent = self.panes.iter().find(|p| p.id == tab.focus).map(Pane::recent_commands).unwrap_or_default();
        let folders = folders.join(" + ");
        if recent.is_empty() {
            folders
        } else {
            format!("{folders} · {}", recent.join(", "))
        }
    }

    /// Reads what every pane's agent is doing, once a frame; a pane whose
    /// agent came or went makes room for its header, or gives it back.
    /// Returns the most urgent state in each tab.
    fn refresh_agents(&mut self) -> Vec<Option<AgentState>> {
        let now = Instant::now();
        let mut moved = false;
        let agents = (0..self.tabs.len())
            .map(|index| {
                let mut leaves = Vec::new();
                self.tabs[index].root.leaves(&mut leaves);
                let seen = index == self.active && self.focused;
                let panes = self.panes.iter_mut().filter(|pane| leaves.contains(&pane.id));
                panes
                    .filter_map(|pane| {
                        if seen {
                            pane.agent_unseen = false;
                        }
                        let state = pane.agent(now);
                        moved |= state.is_some() != pane.agent_now.is_some();
                        pane.agent_now = state;
                        pane.needs_since = match state {
                            Some(need @ (AgentState::Waiting | AgentState::Done)) => match pane.needs_since {
                                Some((was, at)) if was == need => Some((was, at)),
                                _ => Some((need, now)),
                            },
                            _ => None,
                        };
                        let program = pane.header_program();
                        moved |= program.is_some() != pane.program_now.is_some();
                        pane.program_now = program;
                        state
                    })
                    .max()
            })
            .collect();
        if moved {
            self.relayout();
        }
        agents
    }

    /// A working agent's ring turns; any agent is read again soon, and its
    /// header's clock ticks.
    fn wake_for_agents(&mut self, agents: &[Option<AgentState>]) {
        let now = Instant::now();
        let wake = match agents.iter().flatten().max() {
            Some(AgentState::Running | AgentState::Waiting) => Some(now + Duration::from_millis(50)),
            Some(_) => Some(now + Duration::from_millis(400)),
            None => None,
        };
        if let Some(at) = wake {
            self.animation_at = Some(self.animation_at.map_or(at, |a| a.min(at)));
        }
    }

    /// The bar over each pane with an agent: what it's doing and for how
    /// long, what you last asked it, its subagents and its session. A
    /// listed program gets its name and how long it has run.
    fn draw_agent_headers(&mut self, visible: &[(u64, Rect)]) {
        if !self.headers {
            return;
        }
        let now = Instant::now();
        let t = self.ctx.blink_epoch.elapsed().as_secs_f32();
        let focus = self.focused_id();
        for &(id, rect) in visible {
            let Some(pane) = self.panes.iter().find(|pane| pane.id == id) else { continue };
            let info = pane.agent_info(now);
            let program = pane.program_now.clone();
            if (info.is_none() && program.is_none()) || rect.h <= 2.0 * AGENT_HEADER {
                continue;
            }
            let bar = Rect::new(rect.x, rect.y - AGENT_HEADER, rect.w, AGENT_HEADER);
            let Self { scene, ctx, pane_hits, .. } = self;
            let Ctx { text, theme, .. } = ctx;
            match (info, program) {
                (Some(info), _) => draw_agent_header(scene, text, theme, bar, &info, Some(id) == focus, t),
                (None, Some((name, started))) => {
                    draw_program_header(scene, text, theme, bar, &name, now.duration_since(started))
                }
                (None, None) => {}
            }
            pane_hits.push((bar, self.active, id));
        }
    }

    /// The title bar over the window's top, where the system's would be:
    /// the sidebar's color across, the title centered. AppKit keeps the
    /// traffic lights and the dragging; this is only the look.
    fn draw_titlebar(&mut self) {
        let top = self.top();
        if top <= 0.0 {
            return;
        }
        let bar = Rect::new(0.0, 0.0, self.width, top);
        let Self { scene, ctx, title, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.fill(bar, theme.panel(SURFACE_LIFT / 2.0));
        scene.fill(Rect::new(0.0, top - 1.0, bar.w, 1.0), theme.divider);
        // Clear of the traffic lights on the left, centered in what's left.
        // Fixed in points, like the bar: zoom leaves it alone.
        let font = 18.0 / self.zoom;
        let room = (bar.w - 2.0 * 84.0 / self.zoom).max(0.0);
        let shown = text.elide(title, UI_BOLD, font, room);
        let w = text.measure(&shown, UI_BOLD, font);
        let baseline = (top / 2.0 + text.ui_ascent(font) * 0.36).round();
        text.draw_text(scene, &shown, (bar.w - w) / 2.0, baseline, UI_BOLD, font, theme.text_dim);
    }

    fn draw_tab_bar(&mut self, agents: &[Option<AgentState>]) {
        self.tab_hits.clear();
        self.close_hits.clear();
        self.more_hits.clear();
        self.new_tab_hit = Rect::default();
        self.collapse_hit = Rect::default();
        self.extend_hit = Rect::default();
        if !self.tab_list_visible() {
            return;
        }
        let now = Instant::now();
        let extended = self.extended && !self.compact && self.strip == TabStrip::Left;
        let tabs: Vec<TabView> = (0..self.tabs.len())
            .map(|index| {
                let mut leaves = Vec::new();
                self.tabs[index].root.leaves(&mut leaves);
                // Only the current tab grows its lines; the rest stay one row.
                let lines =
                    if extended && index == self.active { self.pane_lines(index, &leaves, now) } else { Vec::new() };
                TabView {
                    lines,
                    agent: agents[index],
                    title: self.tab_title(index),
                    path: self
                        .panes
                        .iter()
                        .find(|pane| pane.id == self.tabs[index].focus)
                        .map(|pane| ui::short_path(&pane.session.cwd))
                        .unwrap_or_default(),
                    panes: leaves.len(),
                    color: self.tabs[index].color.map(|c| Color::hex(TAB_COLORS[c])),
                    mark: self.tabs[index].mark,
                    editing: self
                        .renaming
                        .as_ref()
                        .filter(|rename| rename.tab == index)
                        .map(|rename| (rename.name.clone(), rename.selected)),
                }
            })
            .collect();
        match self.strip {
            TabStrip::Left => self.draw_tab_sidebar(&tabs),
            TabStrip::Top => self.draw_tab_strip(&tabs),
        }
    }

    /// What each of `leaves` is doing, a line apiece: an agent's state, turn
    /// and last prompt; a shell's running command, or its folder.
    fn pane_lines(&mut self, index: usize, leaves: &[u64], now: Instant) -> Vec<PaneLine> {
        let sender = self.ctx.sender.clone();
        for pane in self.panes.iter_mut().filter(|pane| leaves.contains(&pane.id)) {
            pane.refresh_git(&sender, now);
        }
        // Idle lines say the folder only where it isn't the tab's own.
        let focus = self.tabs[index].focus;
        let base = self.panes.iter().find(|pane| pane.id == focus).map(|pane| pane.session.cwd.clone());
        let lines: Vec<PaneLine> = leaves
            .iter()
            .filter_map(|&id| {
                if id == SETTINGS {
                    return Some(PaneLine { id, agent: None, busy: false, failed: false, text: "Settings".to_owned() });
                }
                let pane = self.panes.iter().find(|pane| pane.id == id)?;
                if let Some(info) = pane.agent_info(now) {
                    let mut text = agent_summary(&info);
                    if let Some(prompt) = &info.prompt {
                        text = format!("{text} · {prompt}");
                    }
                    return Some(PaneLine { id, agent: Some(info.state), busy: true, failed: false, text });
                }
                let (busy, text) = pane.activity();
                if busy {
                    return Some(PaneLine { id, agent: None, busy, failed: false, text });
                }
                let text = pane.idle_line(base.as_deref().unwrap_or(&pane.session.cwd), now);
                Some(PaneLine { id, agent: None, busy, failed: pane.last_failed(), text })
            })
            .collect();
        // A lone idle shell: its title says it all.
        if let [line] = lines.as_slice() {
            if !line.busy && line.agent.is_none() {
                return Vec::new();
            }
        }
        // Idle lines tell how long ago; read them again as the minutes turn.
        if lines.iter().any(|line| !line.busy) {
            let at = now + Duration::from_secs(30);
            self.animation_at = Some(self.animation_at.map_or(at, |a| a.min(at)));
        }
        lines
    }

    /// One tab's row: number, title (or the rename field), the
    /// ⋯ menu button while hovered or active, an optional badge, and ×;
    /// records the hit areas.
    fn draw_tab(&mut self, index: usize, tab: &TabView, rect: Rect, badge: Option<String>) {
        let current = index == self.active;
        let hovered = self.hover_tab == Some(index);
        let left = self.strip == TabStrip::Left;
        let (font, number) = if left { (SIDEBAR_FONT, 22.0) } else { (12.0, 18.0) };
        let t = self.ctx.blink_epoch.elapsed().as_secs_f32();
        let Self { scene, ctx, tab_hits, close_hits, more_hits, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let ascent = text.ui_ascent(font);
        let baseline = (rect.y + (rect.h + ascent * 0.72) / 2.0).round();
        let mut x = rect.x + if left { 10.0 } else { 8.0 };
        if index < 9 || tab.agent.is_some() {
            let center = (x + number / 2.0, rect.y + rect.h / 2.0);
            draw_tab_number(scene, text, theme, index, center, number, tab.agent, t);
            x += number + 8.0;
        }
        let close = Rect::new(rect.right() - 22.0, rect.y + (rect.h - 16.0) / 2.0, 16.0, 16.0);
        let mut right = close.x - 4.0;
        if (hovered || current) && tab.editing.is_none() {
            let more = Rect::new(close.x - 20.0, close.y, 18.0, 16.0);
            for i in 0..3 {
                let dot = Rect::new(more.x + 4.0 + i as f32 * 4.0, more.y + 7.0, 2.4, 2.4);
                scene.quad(Quad::new(dot, theme.text_faint).radius(1.2));
            }
            more_hits.push((more, index));
            right = more.x - 2.0;
        }
        if let Some(label) = badge {
            // A small badge with the pane count.
            let w = text.measure(&label, UI, 10.0) + 10.0;
            let badge = Rect::new(right - w, rect.y + (rect.h - 16.0) / 2.0, w, 16.0);
            scene.quad(Quad::new(badge, theme.divider).radius(8.0));
            text.draw_text(scene, &label, badge.x + 5.0, badge.y + 12.0, UI, 10.0, theme.text_dim);
            right = badge.x - 6.0;
        }
        if let Some(mark) = tab.mark {
            let ink = mark.color(theme);
            let w = text.measure(mark.label(), UI_BOLD, 10.0) + 12.0;
            let chip = Rect::new(right - w, rect.y + (rect.h - 16.0) / 2.0, w, 16.0);
            scene.quad(Quad::new(chip, ink.alpha(0.16)).radius(8.0).border(1.0, ink.alpha(0.45)));
            text.draw_text(scene, mark.label(), chip.x + 6.0, chip.y + 12.0, UI_BOLD, 10.0, ink);
            right = chip.x - 6.0;
        }
        if let Some((name, selected)) = &tab.editing {
            let field = Rect::new(x - 4.0, rect.y + 4.0, (close.x - x).max(0.0), rect.h - 8.0);
            scene.quad(Quad::new(field, theme.background).radius(4.0).border(1.0, theme.accent));
            scene.push_clip(field.inset(2.0, 0.0));
            let w = text.measure(name, UI, font);
            if *selected && !name.is_empty() {
                scene.quad(
                    Quad::new(Rect::new(x - 1.0, field.y + 3.0, w + 2.0, field.h - 6.0), theme.selection).radius(2.0),
                );
            }
            text.draw_text(scene, name, x, baseline, UI, font, theme.text);
            scene.fill(Rect::new(x + w + 1.0, field.y + 4.0, 1.0, field.h - 8.0), theme.cursor);
            scene.pop_clip();
        } else {
            scene.push_clip(Rect::new(x, rect.y, (right - x).max(0.0), rect.h));
            let color = if tab.mark.is_some_and(Mark::aside) {
                theme.text_faint
            } else if current {
                theme.text
            } else {
                theme.text_dim
            };
            text.draw_text(scene, &tab.title, x, baseline, UI, font, color);
            scene.pop_clip();
        }
        text.draw_text(scene, "×", close.x + 4.0, baseline, UI, font + 1.0, theme.text_faint);
        tab_hits.push((rect, index));
        close_hits.push((close, index));
    }

    /// Where tab `index` draws, given the `rect` it belongs at: on its way
    /// there while it slides after a reorder.
    fn slid(&mut self, index: usize, rect: Rect) -> Rect {
        let now = Instant::now();
        let Some(tab) = self.tabs.get_mut(index) else { return rect };
        let mut drawn = rect;
        if let Some(((x, y), at)) = tab.slide {
            let t = now.duration_since(at).as_secs_f32() / TAB_SLIDE.as_secs_f32();
            if t >= 1.0 {
                tab.slide = None;
            } else {
                // Ease out: quick to start, settling gently.
                let left = (1.0 - t).powi(3);
                drawn.x += (x - rect.x) * left;
                drawn.y += (y - rect.y) * left;
                let next = now + Duration::from_millis(16);
                self.animation_at = Some(self.animation_at.map_or(next, |a| a.min(next)));
            }
        }
        tab.shown = Some((drawn.x, drawn.y));
        drawn
    }

    /// The tabs as a strip across the top.
    fn draw_tab_strip(&mut self, tabs: &[TabView]) {
        let width = self.width;
        let theme = &self.ctx.theme;
        let top = self.top();
        self.scene.fill(Rect::new(0.0, top, width, TAB_BAR_HEIGHT), theme.panel(SURFACE_LIFT / 2.0));
        self.scene.fill(Rect::new(0.0, top + TAB_BAR_HEIGHT - 1.0, width, 1.0), theme.divider);
        let plus = 34.0;
        let tab_width = ((width - 12.0 - plus) / tabs.len() as f32).min(TAB_MAX_WIDTH);
        for (index, tab) in tabs.iter().enumerate() {
            let place = Rect::new(6.0 + index as f32 * tab_width, top + 5.0, tab_width - 4.0, TAB_BAR_HEIGHT - 9.0);
            let rect = self.slid(index, place);
            draw_tab_background(&mut self.scene, &self.ctx.theme, rect, tab.color, index == self.active, false);
            let badge = (tab.panes > 1).then(|| format!("{}", tab.panes));
            self.draw_tab(index, tab, rect, badge);
            // Hits go by where it belongs, so a drag doesn't chase it.
            if let Some(hit) = self.tab_hits.last_mut() {
                hit.0 = place;
            }
        }
        let Self { scene, ctx, new_tab_hit, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let px = 6.0 + tabs.len() as f32 * tab_width;
        let plus_rect = Rect::new(px, top + 5.0, 26.0, TAB_BAR_HEIGHT - 9.0);
        let ascent = text.ui_ascent(12.0);
        let baseline = (plus_rect.y + (plus_rect.h + ascent) / 2.0).round();
        text.draw_text(scene, "+", px + 8.0, baseline, UI, 15.0, theme.text_dim);
        *new_tab_hit = plus_rect;
    }

    /// The tabs as a list down the left side.
    fn draw_tab_sidebar(&mut self, tabs: &[TabView]) {
        let area = self.tab_area();
        let theme = &self.ctx.theme;
        self.scene.fill(area, theme.panel(SURFACE_LIFT / 2.0));
        self.scene.fill(Rect::new(area.right() - 1.0, area.y, 1.0, area.h), theme.divider);
        self.scene.push_clip(Rect::new(0.0, area.y, area.w - 1.0, area.h));
        let dragged = match self.pointer {
            Pointer::Tab { index, dragging: true, .. } => Some(index),
            _ => None,
        };
        let mut y = area.y + 8.0;
        for (index, tab) in tabs.iter().enumerate() {
            let place = Rect::new(6.0, y, area.w - 13.0, SIDEBAR_ROW);
            let rect = self.slid(index, place);
            if self.compact {
                self.draw_compact_tab(index, tab, rect, dragged == Some(index));
                if let Some(hit) = self.tab_hits.last_mut() {
                    hit.0 = place;
                }
                y += SIDEBAR_ROW + 4.0;
                continue;
            }
            let extra = if tab.lines.is_empty() { 0.0 } else { tab.lines.len() as f32 * SIDEBAR_LINE + 6.0 };
            let row = Rect::new(rect.x, rect.y, rect.w, rect.h + extra);
            draw_tab_background(
                &mut self.scene,
                &self.ctx.theme,
                row,
                tab.color,
                index == self.active,
                dragged == Some(index),
            );
            let badge = (tab.panes > 1 && tab.lines.is_empty()).then(|| format!("{}", tab.panes));
            self.draw_tab(index, tab, rect, badge);
            if let Some(hit) = self.tab_hits.last_mut() {
                hit.0 = Rect::new(place.x, place.y, row.w, row.h);
            }
            self.draw_pane_lines(index, tab, Rect::new(rect.x, rect.bottom() - 4.0, rect.w, extra));
            y += row.h + 4.0;
        }
        let Self { scene, ctx, new_tab_hit, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let plus = Rect::new(6.0, y, area.w - 13.0, SIDEBAR_ROW);
        let ascent = text.ui_ascent(SIDEBAR_FONT);
        let baseline = (plus.y + (plus.h + ascent * 0.72) / 2.0).round();
        if self.compact {
            let w = text.measure("+", UI, 15.0);
            text.draw_text(scene, "+", plus.x + (plus.w - w) / 2.0, baseline, UI, 15.0, theme.text_faint);
        } else {
            let label = format!("+  New tab   {}+T", platform::COMMAND_NAME);
            text.draw_text(scene, &label, plus.x + 16.0, baseline, UI, SIDEBAR_FONT, theme.text_faint);
        }
        *new_tab_hit = plus;
        // Collapse to compact, or expand back, from the bottom corner.
        let collapse = Rect::new(6.0, area.bottom() - 34.0, 28.0, 26.0);
        let collapse =
            if self.compact { Rect::new((area.w - 1.0 - 28.0) / 2.0, collapse.y, 28.0, 26.0) } else { collapse };
        if collapse.contains(self.mouse.0, self.mouse.1) {
            scene.quad(Quad::new(collapse, theme.raised).radius(5.0));
        }
        let glyph = if self.compact { "»" } else { "«" };
        let w = text.measure(glyph, UI, 14.0);
        let baseline = (collapse.y + (collapse.h + ascent * 0.72) / 2.0).round();
        text.draw_text(scene, glyph, collapse.x + (collapse.w - w) / 2.0, baseline, UI, 14.0, theme.text_faint);
        self.collapse_hit = collapse;
        // Beside it, the extended rows' switch: a row with lines under it.
        if !self.compact {
            let extend = Rect::new(collapse.right() + 4.0, collapse.y, 28.0, 26.0);
            if self.extended {
                scene.quad(Quad::new(extend, theme.accent.alpha(0.18)).radius(5.0));
            } else if extend.contains(self.mouse.0, self.mouse.1) {
                scene.quad(Quad::new(extend, theme.raised).radius(5.0));
            }
            let ink = if self.extended { theme.accent } else { theme.text_faint };
            let (x, y) = (extend.x + 8.0, extend.y + 7.0);
            scene.quad(Quad::new(Rect::new(x, y, 12.0, 3.0), ink).radius(1.0));
            for i in 0..2 {
                let line = Rect::new(x + 3.0, y + 6.0 + i as f32 * 4.0, 9.0, 1.5);
                scene.quad(Quad::new(line, ink.alpha(0.8)).radius(0.75));
            }
            self.extend_hit = extend;
        }
        scene.pop_clip();
        self.draw_tab_hint(tabs);
    }

    /// The extended sidebar's lines under a tab's title, one per pane:
    /// a dot for what it's doing, then what that is.
    fn draw_pane_lines(&mut self, index: usize, tab: &TabView, area: Rect) {
        let t = self.ctx.blink_epoch.elapsed().as_secs_f32();
        let focus = self.tabs.get(index).map(|tab| tab.focus);
        let Self { scene, ctx, pane_hits, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let font = 11.5;
        let ascent = text.ui_ascent(font);
        for (i, line) in tab.lines.iter().enumerate() {
            let rect = Rect::new(area.x, area.y + i as f32 * SIDEBAR_LINE, area.w, SIDEBAR_LINE);
            let cy = rect.y + rect.h / 2.0;
            let dot = Rect::new(rect.x + 16.0, cy - 3.0, 6.0, 6.0);
            let color = match line.agent {
                Some(state) => agent_color(theme, state, t),
                None if line.busy => theme.accent.alpha(0.7),
                None if line.failed => theme.error.alpha(0.85),
                None => theme.text_faint.alpha(0.5),
            };
            scene.quad(Quad::new(dot, color).radius(3.0));
            let x = rect.x + 30.0;
            let baseline = (cy + ascent * 0.36).round();
            let ink = if Some(line.id) == focus && tab.panes > 1 { theme.text } else { theme.text_dim };
            let room = (rect.right() - 8.0 - x).max(0.0);
            let shown = text.elide(&line.text, UI, font, room);
            text.draw_text(scene, &shown, x, baseline, UI, font, ink);
            pane_hits.push((rect, index, line.id));
        }
    }

    /// A compact sidebar row: the tab's number on its background.
    fn draw_compact_tab(&mut self, index: usize, tab: &TabView, rect: Rect, dragged: bool) {
        let Self { scene, ctx, tab_hits, .. } = self;
        let Ctx { text, theme, blink_epoch, .. } = ctx;
        let current = index == self.active;
        draw_tab_background(scene, theme, rect, tab.color, current, dragged);
        let center = (rect.x + rect.w / 2.0 + 1.0, rect.y + rect.h / 2.0);
        let t = blink_epoch.elapsed().as_secs_f32();
        draw_tab_number(scene, text, theme, index, center, 24.0, tab.agent, t);
        if let Some(mark) = tab.mark {
            let dot = Rect::new(rect.right() - 9.0, rect.y + 3.0, 6.0, 6.0);
            scene.quad(Quad::new(dot, mark.color(theme)).radius(3.0));
        }
        tab_hits.push((rect, index));
    }

    /// A label beside the hovered row: a compact row's title at once, a
    /// full row's folder after a moment, when its title doesn't show it.
    fn draw_tab_hint(&mut self, tabs: &[TabView]) {
        if !matches!(self.pointer, Pointer::Idle) {
            return;
        }
        let (mx, my) = self.mouse;
        let Some(&(row, index)) = self.tab_hits.iter().find(|(r, _)| r.contains(mx, my)) else { return };
        let Some(tab) = tabs.get(index) else { return };
        let title = if self.compact {
            match tab.agent {
                Some(agent) => format!("{} · {}", agent.label(), tab.title),
                None => tab.title.clone(),
            }
        } else {
            if tab.path.is_empty() || tab.path == tab.title || tab.editing.is_some() {
                return;
            }
            let at = self.hover_at + TAB_HINT_DELAY;
            if Instant::now() < at {
                self.animation_at = Some(self.animation_at.map_or(at, |a| a.min(at)));
                return;
            }
            tab.path.clone()
        };
        let Self { scene, ctx, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.set_layer(LAYER_OVERLAY);
        let w = (text.measure(&title, UI, 12.0) + 20.0).min(TAB_MAX_WIDTH * 1.5);
        let label = Rect::new(row.right() + 8.0, row.y + 2.0, w, row.h - 4.0);
        scene.shadow(label, Color::hex(0x000000).alpha(0.5), 8.0, 6.0);
        scene.quad(Quad::new(label, theme.raised).radius(6.0).border(1.0, theme.divider));
        scene.push_clip(label.inset(8.0, 0.0));
        let baseline = (label.y + (label.h + text.ui_ascent(12.0) * 0.72) / 2.0).round();
        text.draw_text(scene, &title, label.x + 10.0, baseline, UI, 12.0, theme.text);
        scene.pop_clip();
        scene.set_layer(LAYER_CONTENT);
    }

    /// The open tab menu: Rename, a row of colors, Close.
    fn draw_tab_menu(&mut self) {
        self.menu_hits.clear();
        self.menu_rect = Rect::default();
        let Some(menu) = &self.tab_menu else { return };
        if menu.pane {
            return self.draw_pane_menu();
        }
        let Some(tab) = self.tabs.get(menu.tab) else { return };
        let selected = tab.color;
        let marked = tab.mark;
        let several = self.tabs.len() > 1;
        let rows = if several { 5.0 } else { 4.0 };
        let h = rows * MENU_ROW + 12.0 + 9.0;
        let x = menu.x.min(self.width - MENU_WIDTH - 8.0).max(4.0);
        let y = menu.y.min(self.height - h - 8.0).max(4.0);
        let rect = Rect::new(x, y, MENU_WIDTH, h);
        let (mx, my) = self.mouse;
        let Self { scene, ctx, menu_hits, menu_rect, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.set_layer(LAYER_OVERLAY);
        scene.shadow(rect, Color::hex(0x000000).alpha(0.5), 8.0, 10.0);
        scene.quad(Quad::new(rect, theme.raised).radius(8.0).border(1.0, theme.divider));
        let ascent = text.ui_ascent(12.0);
        let mut row_y = y + 6.0;
        let mut item = |scene: &mut Scene, label: &str, action: MenuAction, row_y: f32| {
            let row = Rect::new(x + 5.0, row_y, MENU_WIDTH - 10.0, MENU_ROW);
            if row.contains(mx, my) {
                scene.quad(Quad::new(row, theme.accent.alpha(0.22)).radius(5.0));
            }
            let baseline = (row.y + (row.h + ascent * 0.72) / 2.0).round();
            text.draw_text(scene, label, row.x + 9.0, baseline, UI, 12.0, theme.text);
            menu_hits.push((row, action));
        };
        item(scene, "Rename Tab…", MenuAction::Rename, row_y);
        row_y += MENU_ROW;
        // The colors: none, then each of TAB_COLORS.
        let size = 16.0;
        let step = (MENU_WIDTH - 28.0 - size) / TAB_COLORS.len() as f32;
        let mut swatches = Vec::new();
        for choice in std::iter::once(None).chain((0..TAB_COLORS.len()).map(Some)) {
            let i = choice.map_or(0.0, |c| c as f32 + 1.0);
            let swatch = Rect::new(x + 14.0 + i * step, row_y + (MENU_ROW - size) / 2.0, size, size);
            let hit = swatch.inset(-3.0, -3.0);
            let ring = if selected == choice {
                theme.text
            } else if hit.contains(mx, my) {
                theme.text_dim
            } else {
                theme.divider
            };
            match choice {
                Some(c) => {
                    scene.quad(Quad::new(swatch, Color::hex(TAB_COLORS[c])).radius(size / 2.0).border(1.5, ring))
                }
                None => {
                    scene.quad(Quad::new(swatch, theme.background).radius(size / 2.0).border(1.5, ring));
                    let slash = Rect::new(swatch.x + size / 2.0 - 0.75, swatch.y + 3.0, 1.5, size - 6.0);
                    scene.fill(slash, theme.text_faint);
                }
            }
            swatches.push((hit, MenuAction::Color(choice)));
        }
        row_y += MENU_ROW;
        // The marks go here, drawn once the rows are done with the text.
        let marks_y = row_y;
        row_y += MENU_ROW + 4.0;
        scene.fill(Rect::new(x + 10.0, row_y, MENU_WIDTH - 20.0, 1.0), theme.divider);
        row_y += 5.0;
        item(scene, "Close Tab", MenuAction::Close, row_y);
        if several {
            item(scene, "Close Other Tabs", MenuAction::CloseOthers, row_y + MENU_ROW);
        }
        // The marks, each a chip that toggles.
        let mut chip_x = x + 10.0;
        for mark in Mark::ALL {
            let ink = mark.color(theme);
            let w = text.measure(mark.label(), UI_BOLD, 10.5) + 14.0;
            let chip = Rect::new(chip_x, marks_y + (MENU_ROW - 18.0) / 2.0, w, 18.0);
            let on = marked == Some(mark);
            let fill = if on {
                ink.alpha(0.3)
            } else if chip.contains(mx, my) {
                ink.alpha(0.14)
            } else {
                theme.background
            };
            scene.quad(Quad::new(chip, fill).radius(9.0).border(1.0, ink.alpha(if on { 0.9 } else { 0.4 })));
            text.draw_text(scene, mark.label(), chip.x + 7.0, chip.y + 13.0, UI_BOLD, 10.5, ink);
            swatches.push((chip, MenuAction::Mark(mark)));
            chip_x += w + 6.0;
        }
        menu_hits.extend(swatches);
        scene.set_layer(LAYER_CONTENT);
        *menu_rect = rect;
    }

    /// The focused pane's menu: Copy, Paste, Clear, the splits, Close.
    fn draw_pane_menu(&mut self) {
        let Some(menu) = &self.tab_menu else { return };
        let cmd = platform::COMMAND_NAME;
        let rows: [(&str, String, MenuAction); 6] = [
            ("Copy", format!("{cmd}C"), MenuAction::Copy),
            ("Paste", format!("{cmd}V"), MenuAction::Paste),
            ("Clear", format!("{cmd}K"), MenuAction::Clear),
            ("Split Right", format!("{cmd}D"), MenuAction::SplitRight),
            ("Split Down", format!("{cmd}E"), MenuAction::SplitDown),
            ("Close Pane", format!("{cmd}W"), MenuAction::ClosePane),
        ];
        let h = rows.len() as f32 * MENU_ROW + 12.0 + 2.0 * 9.0;
        let x = menu.x.min(self.width - MENU_WIDTH - 8.0).max(4.0);
        let y = menu.y.min(self.height - h - 8.0).max(4.0);
        let rect = Rect::new(x, y, MENU_WIDTH, h);
        let (mx, my) = self.mouse;
        let Self { scene, ctx, menu_hits, menu_rect, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.set_layer(LAYER_OVERLAY);
        scene.shadow(rect, Color::hex(0x000000).alpha(0.5), 8.0, 10.0);
        scene.quad(Quad::new(rect, theme.raised).radius(8.0).border(1.0, theme.divider));
        let ascent = text.ui_ascent(12.0);
        let mut row_y = y + 6.0;
        for (i, (label, shortcut, action)) in rows.into_iter().enumerate() {
            if i == 3 || i == 5 {
                scene.fill(Rect::new(x + 10.0, row_y + 4.0, MENU_WIDTH - 20.0, 1.0), theme.divider);
                row_y += 9.0;
            }
            let row = Rect::new(x + 5.0, row_y, MENU_WIDTH - 10.0, MENU_ROW);
            if row.contains(mx, my) {
                scene.quad(Quad::new(row, theme.accent.alpha(0.22)).radius(5.0));
            }
            let baseline = (row.y + (row.h + ascent * 0.72) / 2.0).round();
            text.draw_text(scene, label, row.x + 9.0, baseline, UI, 12.0, theme.text);
            let w = text.measure(&shortcut, UI, 11.0);
            text.draw_text(scene, &shortcut, row.right() - 9.0 - w, baseline, UI, 11.0, theme.text_faint);
            menu_hits.push((row, action));
            row_y += MENU_ROW;
        }
        scene.set_layer(LAYER_CONTENT);
        *menu_rect = rect;
    }

    /// An offered update: a pill at the foot of the sidebar, and above it
    /// the card with the release's version, size and date.
    fn draw_update(&mut self) {
        self.update_hits.clear();
        self.update_card = Rect::default();
        let Some(offer) = &self.update else { return };
        let sidebar = self.tab_list_visible() && self.strip == TabStrip::Left;
        let area = self.tab_area();
        let (mx, my) = self.mouse;
        let height = self.height;
        let Self { scene, ctx, update_hits, update_card, compact, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let ascent = text.ui_ascent(12.0);
        // The baseline that centers `size` text in `rect`.
        let centered = |rect: Rect, size: f32| (rect.y + (rect.h + ascent * size / 12.0 * 0.72) / 2.0).round();
        // A deeper blue than the accent, so white text reads on it.
        let blue = theme.accent.mix(Color::hex(0x0a4fd6), 0.65);
        let blue_hover = |rect: Rect| if rect.contains(mx, my) { blue.mix(Color::hex(0xffffff), 0.12) } else { blue };

        // The pill, beside the sidebar's collapse button (above it when compact).
        let pill = if !sidebar {
            None
        } else if *compact {
            Some(Rect::new((area.w - 1.0 - 28.0) / 2.0, area.bottom() - 64.0, 28.0, 26.0))
        } else {
            let label = "Update Available…";
            let w = (text.measure(label, UI_BOLD, 12.0) + 38.0).min(area.w - 48.0);
            Some(Rect::new(40.0, area.bottom() - 34.0, w, 26.0))
        };
        if let Some(pill) = pill {
            scene.quad(Quad::new(pill, blue_hover(pill)).radius(pill.h / 2.0));
            let baseline = centered(pill, 12.0);
            if *compact {
                let w = text.measure("↑", UI_BOLD, 13.0);
                text.draw_text(scene, "↑", pill.x + (pill.w - w) / 2.0, baseline, UI_BOLD, 13.0, Color::hex(0xffffff));
            } else {
                scene.push_clip(pill.inset(10.0, 0.0));
                text.draw_text(scene, "↑", pill.x + 12.0, baseline, UI_BOLD, 12.0, Color::hex(0xffffff));
                text.draw_text(
                    scene,
                    "Update Available…",
                    pill.x + 26.0,
                    baseline,
                    UI_BOLD,
                    12.0,
                    Color::hex(0xffffff),
                );
                scene.pop_clip();
            }
            update_hits.push((pill, UpdateAction::Toggle));
        }
        if !offer.open {
            return;
        }

        const W: f32 = 290.0;
        const PAD: f32 = 14.0;
        const ROW: f32 = 18.0;
        const BUTTON: f32 = 24.0;
        const FOOTER: f32 = 34.0;
        let h = 40.0 + 3.0 * ROW + 12.0 + BUTTON + 14.0 + FOOTER;
        let (x, bottom) = match pill {
            Some(pill) if *compact => (pill.right() + 10.0, pill.bottom() + 8.0),
            Some(pill) => (8.0, pill.y - 9.0),
            None => (12.0, height - 12.0),
        };
        let card = Rect::new(x, bottom - h, W, h);
        scene.set_layer(LAYER_OVERLAY);
        scene.shadow(card, Color::hex(0x000000).alpha(0.55), 10.0, 14.0);
        scene.quad(Quad::new(card, theme.raised).radius(10.0).border(1.0, theme.divider));
        // The card's tail, pointing at the pill.
        if let Some(pill) = pill.filter(|_| !*compact) {
            let cx = pill.x + pill.w / 2.0;
            for i in 0..7 {
                let half = 7.0 - i as f32;
                scene.fill(Rect::new(cx - half, card.bottom() - 1.0 + i as f32, half * 2.0, 1.0), theme.raised);
            }
        }

        text.draw_text(scene, "Update Available", card.x + PAD, card.y + 26.0, UI_BOLD, 13.0, theme.text);
        let release = &offer.release;
        let rows = [
            ("Version:", release.version.clone()),
            ("Size:", release.size_label()),
            ("Released:", release.date_label()),
        ];
        let column = card.x + PAD + text.measure("Released:", UI, 11.5) + 8.0;
        let mut y = card.y + 40.0;
        for (label, value) in rows {
            let baseline = y + 13.0;
            let w = text.measure(label, UI, 11.5);
            text.draw_text(scene, label, column - 6.0 - w, baseline, UI, 11.5, theme.text_dim);
            text.draw_text(scene, &value, column, baseline, UI, 11.5, theme.text);
            y += ROW;
        }

        // Skip and Later on the left, the install on the right.
        y += 12.0;
        let busy = offer.installing;
        let mut button = |scene: &mut Scene, x: f32, label: &str, action: UpdateAction| -> f32 {
            let w = text.measure(label, UI, 11.5) + 22.0;
            let rect = Rect::new(x, y, w, BUTTON);
            let hover = rect.contains(mx, my) && !busy;
            let fill = if hover {
                theme.raised.mix(Color::hex(0xffffff), 0.1)
            } else {
                theme.raised.mix(Color::hex(0xffffff), 0.05)
            };
            scene.quad(Quad::new(rect, fill).radius(6.0).border(1.0, theme.divider));
            let color = if busy { theme.text_faint } else { theme.text };
            text.draw_text(scene, label, rect.x + 11.0, centered(rect, 11.5), UI, 11.5, color);
            update_hits.push((rect, action));
            w
        };
        let skip = button(scene, card.x + PAD, "Skip", UpdateAction::Skip);
        button(scene, card.x + PAD + skip + 8.0, "Later", UpdateAction::Later);
        let label = if busy { "Installing…" } else { "Install and Relaunch" };
        let w = text.measure(label, UI_BOLD, 11.5) + 24.0;
        let install = Rect::new(card.right() - PAD - w, y, w, BUTTON);
        let fill = if busy { blue.alpha(0.6) } else { blue_hover(install) };
        scene.quad(Quad::new(install, fill).radius(6.0));
        text.draw_text(scene, label, install.x + 12.0, centered(install, 11.5), UI_BOLD, 11.5, Color::hex(0xffffff));
        update_hits.push((install, UpdateAction::Install));

        // View Release Notes, under a rule.
        y += BUTTON + 14.0;
        scene.fill(Rect::new(card.x, y, card.w, 1.0), theme.divider);
        let footer = Rect::new(card.x + 1.0, y + 1.0, card.w - 2.0, FOOTER - 2.0);
        if footer.contains(mx, my) {
            scene.quad(Quad::new(footer, theme.text.alpha(0.05)).radius(9.0));
        }
        let baseline = centered(footer, 11.5);
        text.draw_text(scene, "View Release Notes", footer.x + PAD - 1.0, baseline, UI_BOLD, 11.5, theme.text);
        let w = text.measure("↗", UI, 12.0);
        text.draw_text(scene, "↗", footer.right() - PAD - w, baseline, UI, 12.0, theme.text_dim);
        update_hits.push((footer, UpdateAction::Notes));
        scene.set_layer(LAYER_CONTENT);
        *update_card = card;
    }

    fn draw_toast(&mut self) {
        let Some((message, _)) = &self.ctx.toast else { return };
        let message = message.clone();
        let (right, bottom) = match self.focused_id().and_then(|id| self.panes.iter().find(|p| p.id == id)) {
            Some(pane) => (pane.rect.right(), pane.rect.y + pane.input_top()),
            None => (self.width, self.height),
        };
        let Self { scene, ctx, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.set_layer(LAYER_OVERLAY);
        let w = text.measure(&message, UI, 12.0) + 24.0;
        let rect = Rect::new(right - w - 14.0, bottom - 40.0, w, 28.0);
        scene.shadow(rect, Color::hex(0x000000).alpha(0.5), 14.0, 8.0);
        scene.quad(Quad::new(rect, theme.raised).radius(14.0).border(1.0, theme.divider));
        text.draw_text(scene, &message, rect.x + 12.0, rect.y + 18.0, UI, 12.0, theme.text);
        scene.set_layer(LAYER_CONTENT);
    }
}

/// How long a pane sits idle before coming back to it shows a recap:
/// `TEMINO_RECAP_MINUTES`, else the `recap_minutes` setting, else 10.
/// Zero turns it off (Command+I still shows one).
fn recap_idle() -> Duration {
    let minutes = std::env::var("TEMINO_RECAP_MINUTES")
        .ok()
        .or_else(|| crate::setting("recap_minutes"))
        .and_then(|minutes| minutes.parse::<f32>().ok())
        .unwrap_or(10.0);
    Duration::from_secs_f32(minutes.max(0.0) * 60.0)
}

fn tabs_path() -> String {
    format!("{}/tabs", crate::settings_dir())
}

/// TEMINO_CAPTURE: saves each frame, straight from the GPU readback, for
/// screenshots that no other window or the pointer can get into.
/// A tab's background, in the sidebar or the strip: a tab with a color is tinted with it all
/// over, more strongly and outlined in it while active; an uncolored tab
/// is raised only while active.
fn draw_tab_background(
    scene: &mut Scene,
    theme: &Theme,
    rect: Rect,
    color: Option<Color>,
    current: bool,
    dragged: bool,
) {
    let (fill, border) = match (color, current) {
        (Some(color), true) => (theme.raised.mix(color, 0.42), color.mix(theme.text, 0.15)),
        (Some(color), false) => (color.alpha(0.22), color.alpha(0.0)),
        (None, true) => (theme.raised, theme.divider),
        (None, false) => return,
    };
    let fill = if dragged { fill.mix(theme.text, 0.06) } else { fill };
    scene.quad(Quad::new(rect, fill).radius(6.0).border(1.0, border));
}

/// A tab's number in a circle `size` across, ringed and colored by what its
/// agent is doing: a faint ring at rest, a turning dot while it works, amber
/// while it waits on you, green when it finished a turn you haven't seen.
/// Past 9 there is no number, only the circle. `t` drives the motion.
#[allow(clippy::too_many_arguments)]
fn draw_tab_number(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    index: usize,
    (cx, cy): (f32, f32),
    size: f32,
    agent: Option<AgentState>,
    t: f32,
) {
    let r = size / 2.0;
    let circle = Rect::new(cx - r, cy - r, size, size);
    let clear = Color::hex(0x000000).alpha(0.0);
    let ink = Color::hex(0x1b1d22);
    let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU / 1.4).sin();
    let color = match agent {
        None => theme.text_faint,
        Some(AgentState::Idle) => {
            scene.quad(Quad::new(circle, clear).radius(r).border(1.0, theme.text_faint.alpha(0.7)));
            theme.text_dim
        }
        Some(AgentState::Running) => {
            scene.quad(Quad::new(circle, theme.accent.alpha(0.12)).radius(r).border(1.5, theme.accent.alpha(0.3)));
            let angle = t * std::f32::consts::TAU / 1.1;
            let (dx, dy) = ((r - 0.75) * angle.cos(), (r - 0.75) * angle.sin());
            scene.quad(Quad::new(Rect::new(cx + dx - 2.5, cy + dy - 2.5, 5.0, 5.0), theme.accent).radius(2.5));
            theme.text
        }
        Some(AgentState::Waiting) => {
            let amber = Color::hex(AGENT_WAITING);
            let halo = circle.inset(-1.0 - 3.0 * pulse, -1.0 - 3.0 * pulse);
            scene.quad(Quad::new(halo, clear).radius(halo.w / 2.0).border(1.5, amber.alpha(0.5 * (1.0 - pulse))));
            scene.quad(Quad::new(circle, amber.alpha(0.8 + 0.2 * pulse)).radius(r));
            ink
        }
        Some(AgentState::Done) => {
            scene.quad(Quad::new(circle, Color::hex(AGENT_DONE)).radius(r));
            ink
        }
    };
    if index < 9 {
        let label = format!("{}", index + 1);
        let font = (size * 0.5 + 2.0).round();
        let w = text.measure(&label, UI, font);
        let baseline = (cy + text.ui_ascent(font) * 0.36).round();
        text.draw_text(scene, &label, cx - w / 2.0, baseline, UI, font, color);
    } else if agent.is_some() {
        scene.quad(Quad::new(Rect::new(cx - 2.0, cy - 2.0, 4.0, 4.0), color).radius(2.0));
    }
}

/// An agent state's color: the accent while it works, amber while it
/// waits on you, green when it finished a turn you haven't seen.
fn agent_color(theme: &Theme, state: AgentState, t: f32) -> Color {
    let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU / 1.4).sin();
    match state {
        AgentState::Idle => theme.text_faint,
        AgentState::Running => theme.accent.alpha(0.55 + 0.45 * pulse),
        AgentState::Waiting => Color::hex(AGENT_WAITING).alpha(0.7 + 0.3 * pulse),
        AgentState::Done => Color::hex(AGENT_DONE),
    }
}

/// "Running 2m 14s", "Waiting for you", "Done in 3m 02s", "Idle".
fn agent_summary(info: &AgentInfo) -> String {
    match (info.state, info.turn) {
        (AgentState::Running, Some(turn)) => format!("Running {}", ui::short_duration(turn)),
        (AgentState::Done, Some(turn)) => format!("Done in {}", ui::short_duration(turn)),
        (state, _) => state.label().to_owned(),
    }
}

/// An agent's mark, 14 points square, its left edge at `x` and centered on
/// `cy`; returns its right edge. Claude Code's is its spark, drawn as rays
/// of dots; the others are their initial on a badge in their color.
fn draw_agent_icon(scene: &mut Scene, text: &mut TextSystem, program: &str, brand: Color, x: f32, cy: f32) -> f32 {
    let size = 14.0;
    if program.starts_with("claude") {
        let (cx, r) = (x + size / 2.0, size / 2.0);
        for ray in 0..8 {
            let angle = ray as f32 * std::f32::consts::FRAC_PI_4 + 0.2;
            let (sin, cos) = angle.sin_cos();
            for (along, dot) in [(1.6, 2.6), (3.6, 2.4), (5.6, 2.0)] {
                let along = along * r / 7.0;
                let (dx, dy) = (cx + cos * along, cy + sin * along);
                scene.quad(Quad::new(Rect::new(dx - dot / 2.0, dy - dot / 2.0, dot, dot), brand).radius(dot / 2.0));
            }
        }
    } else {
        let badge = Rect::new(x, cy - size / 2.0, size, size);
        scene.quad(Quad::new(badge, brand).radius(4.0));
        let initial = agent::agent_brand(program).0.chars().next().unwrap_or('?').to_uppercase().to_string();
        let w = text.measure(&initial, UI_BOLD, 10.0);
        let baseline = (cy + text.ui_ascent(10.0) * 0.36).round();
        text.draw_text(scene, &initial, x + (size - w) / 2.0, baseline, UI_BOLD, 10.0, Color::hex(0xFFFFFF));
    }
    x + size
}

/// The bar over an agent's pane. Left: the agent's mark and name, then a
/// dot and what it's doing, with the turn's clock; then your last prompt.
/// Right: its subagents, the prompts queued behind this turn, its
/// session's name and how long it has run.
fn draw_agent_header(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    bar: Rect,
    info: &AgentInfo,
    focused: bool,
    t: f32,
) {
    let color = agent_color(theme, info.state, t);
    let fill = theme.panel(SURFACE_LIFT).mix(color, if info.state == AgentState::Waiting { 0.12 } else { 0.04 });
    scene.fill(bar, fill);
    scene.fill(Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0), theme.divider);
    if focused && info.state != AgentState::Idle {
        scene.fill(Rect::new(bar.x, bar.bottom() - 2.0, bar.w, 2.0), color.alpha(0.6));
    }
    let font = 12.0;
    let baseline = (bar.y + bar.h / 2.0 + text.ui_ascent(font) * 0.36).round();
    let cy = bar.y + bar.h / 2.0;
    let mut x = bar.x + 10.0;
    if let Some(program) = info.program {
        let (name, brand) = agent::agent_brand(program);
        x = draw_agent_icon(scene, text, program, Color::hex(brand), x, cy) + 6.0;
        text.draw_text(scene, name, x, baseline, UI_BOLD, font, theme.text);
        x += text.measure(name, UI_BOLD, font) + 12.0;
        scene.fill(Rect::new(x, cy - 7.0, 1.0, 14.0), theme.divider);
        x += 12.0;
    }
    scene.quad(Quad::new(Rect::new(x, cy - 4.0, 8.0, 8.0), color).radius(4.0));
    x += 16.0;
    let summary = agent_summary(info);
    let ink = match info.state {
        AgentState::Waiting => Color::hex(AGENT_WAITING),
        AgentState::Done => Color::hex(AGENT_DONE),
        _ => theme.text,
    };
    text.draw_text(scene, &summary, x, baseline, UI_BOLD, font, ink);
    x += text.measure(&summary, UI_BOLD, font) + 14.0;
    // The right side, laid out from the edge in: the subagents whenever
    // they fit, the rest only while the prompt keeps some room.
    let mut right = bar.right() - 12.0;
    let subagents = (info.subagents > 0)
        .then(|| format!("{} subagent{}", info.subagents, if info.subagents == 1 { "" } else { "s" }));
    let queued = (info.queued > 0).then(|| format!("{} queued", info.queued));
    for chip in subagents.iter().chain(&queued) {
        let w = text.measure(chip, UI, 11.0);
        if right - w - 12.0 > x {
            let pill = Rect::new(right - w - 12.0, cy - 9.0, w + 12.0, 18.0);
            scene.quad(Quad::new(pill, theme.accent.alpha(0.16)).radius(9.0));
            text.draw_text(scene, chip, pill.x + 6.0, baseline, UI, 11.0, theme.accent);
            right = pill.x - 12.0;
        }
    }
    let uptime = format!("up {}", ui::short_duration(info.uptime));
    for chip in info.name.iter().chain(std::iter::once(&uptime)) {
        let w = text.measure(chip, UI, 11.0);
        if right - w < x + 160.0 {
            break;
        }
        text.draw_text(scene, chip, right - w, baseline, UI, 11.0, theme.text_faint);
        right -= w + 14.0;
    }
    if let Some(prompt) = &info.prompt {
        let room = right - x - 14.0;
        let shown = text.elide(&format!("› {prompt}"), UI, font, room);
        text.draw_text(scene, &shown, x, baseline, UI, font, theme.text_dim);
    }
}

/// The bar over a listed program's pane: its name, and how long it has run.
fn draw_program_header(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    bar: Rect,
    name: &str,
    uptime: Duration,
) {
    scene.fill(bar, theme.panel(SURFACE_LIFT));
    scene.fill(Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0), theme.divider);
    let font = 12.0;
    let baseline = (bar.y + bar.h / 2.0 + text.ui_ascent(font) * 0.36).round();
    let cy = bar.y + bar.h / 2.0;
    let x = bar.x + 12.0;
    scene.quad(Quad::new(Rect::new(x, cy - 4.0, 8.0, 8.0), theme.text_faint).radius(4.0));
    text.draw_text(scene, name, x + 16.0, baseline, UI_BOLD, font, theme.text);
    let uptime = format!("up {}", ui::short_duration(uptime));
    let w = text.measure(&uptime, UI, 11.0);
    text.draw_text(scene, &uptime, bar.right() - 12.0 - w, baseline, UI, 11.0, theme.text_faint);
}

fn write_ppm(path: &std::ffi::OsStr, bgra: &[u8], width: u32, height: u32) {
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    ppm.reserve(bgra.len() / 4 * 3);
    for pixel in bgra.chunks_exact(4) {
        ppm.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    let _ = std::fs::write(path, ppm);
}

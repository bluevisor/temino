//! The core thread: owns the renderer and the window's tabs, each a tree
//! of split panes; routes window events and pty output to panes, and draws
//! frames on demand.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::agent::{self, AgentState};
use crate::bindings::{self, Bind, Bindings};
use crate::complete::Commands;
use crate::editor::History;
use crate::fetch::{self, FETCHES};
use crate::gpu::Gpu;
use crate::keys::Named;
use crate::pane::{AgentInfo, Ctx, MarkNote, Pane};
use crate::picture::Picture;
use crate::platform::{self, buttons, Cursor, KeyEvent, UiEvent};
use crate::remote;
use crate::scene::{Quad, Rect, Scene, LAYER_CONTENT, LAYER_OVERLAY};
use crate::settings::{self, Action, Field};
use crate::shells::{self, KINDS};
use crate::text::{Style, TextSystem, UI, UI_BOLD};
use crate::theme::{self, Color, Palette, Theme, BACKGROUNDS, OPACITIES, SURFACE_LIFT};
use crate::ui;
use crate::update::{self, Release};
use crate::viewer::{self, Viewer};
use crate::Msg;

mod chrome;
mod control;
mod draw;
mod input;
mod layout;
mod overlays;
mod persist;
mod prefs;
mod start;
mod tabs;
use chrome::*;
pub use overlays::*;
use prefs::*;
pub use start::*;
use tabs::*;

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
/// How far a row or button under the pointer is lifted from its ground.
const HOVER_LIFT: f32 = 0.055;
/// Sidebar rows read a little larger than the strip's tabs.
const SIDEBAR_FONT: f32 = 13.5;
/// An agent waiting on you, and one that finished a turn you haven't seen.
const AGENT_WAITING: u32 = 0xf0a35e;
const AGENT_DONE: u32 = 0x8ccf7e;
/// The bar over a pane with a coding agent in it.
const AGENT_HEADER: f32 = 26.0;
/// The extended sidebar's line per pane, under each tab's title.
const SIDEBAR_LINE: f32 = 18.0;
/// A sidebar row's line under its title: the pane count and the mark.
const SIDEBAR_META: f32 = 18.0;
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
/// A divider dragged this close to the middle of its split snaps to it.
const MIDDLE_SNAP: f32 = 32.0;
/// How far a press on a tab moves before it becomes a drag.
const DRAG_SLOP: f32 = 4.0;
/// How long a reordered tab takes to slide into its new place.
const TAB_SLIDE: Duration = Duration::from_millis(160);
/// How long a menu or a toast takes to fade in, and how far it travels
/// into place as it does.
const APPEAR: Duration = Duration::from_millis(140);
const APPEAR_TRAVEL: f32 = 5.0;
/// How long a toast takes to fade away.
const TOAST_FADE: Duration = Duration::from_millis(220);
const MENU_WIDTH: f32 = 200.0;
const MENU_ROW: f32 = 26.0;

/// How often a window's state is rebuilt for remotes watching it.
const REMOTE_EVERY: Duration = Duration::from_millis(150);
/// A tab's AI title and summary: asked once what its panes do has held
/// still this long, and at most this often.
const TITLE_SETTLE: Duration = Duration::from_secs(3);
const TITLE_EVERY: Duration = Duration::from_secs(20);
const TITLE_SYSTEM: &str = "You see what is going on in the panes of one terminal tab: their folders, \
the coding agents and what they were asked, the commands run and the end of their output. Answer with \
exactly two lines. Line 1: a title for the tab in 2 to 5 words, the way a person would label it \
(for example: Temino tab colors, API deploy logs, Haiku VM build). Line 2: at most 16 words on what \
is being done in this tab and where it stands, starting with a verb and naming no one (for example: \
Fixing the login redirect bug; tests passing, PR not opened yet). Plain text, no labels, no quotes.";
/// How much of each pane's history a tab's summary is written from, in
/// all.
const TITLE_CHARS: usize = 8000;
/// The colors a tab can be marked with, from its menu: a row of
/// SWATCHES, then a row of alternates shown while Option is held. Each
/// alternate is its color's lighter sibling (hue turned 14°, a little
/// lighter), so the two rows read as one palette.
const TAB_COLORS: [u32; 12] = [
    0xf2737a, 0xf0a35e, 0xe5c76b, 0x8ccf7e, 0x67b0e8, 0xc47fd5, // red … purple
    0xf5a18f, 0xf3cf7a, 0xeae885, 0x94d798, 0x82a5ec,
    0xdd96da, // salmon, apricot, lemon, mint, periwinkle, orchid
];
const SWATCHES: usize = 6;

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

#[derive(Clone)]
struct Divider {
    line: Rect,
    axis: Axis,
    /// The area the split divides, to turn the pointer into a ratio.
    parent: Rect,
    path: Vec<bool>,
}

/// A tab title request's reply: none yet, then the title and summary (or
/// nothing usable).
type TitleAnswer = Arc<std::sync::Mutex<Option<Option<(String, Option<String>)>>>>;

/// A tab's title (used while it has no name) and one-line summary,
/// written by the model from what its panes are doing. Asked again once
/// what they're doing has changed and settled, and not more often than
/// TITLE_EVERY; the old ones stay meanwhile.
#[derive(Default)]
struct AiTitle {
    title: Option<String>,
    summary: Option<String>,
    /// What the last ask was written from, and its answer on the way.
    asked_for: String,
    asked_at: Option<Instant>,
    answer: Option<TitleAnswer>,
    /// What the panes are doing now, and since when.
    seen: String,
    seen_at: Option<Instant>,
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
    ai_title: AiTitle,
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
    BrowserPane,
    ClosePane,
    // The account menu, from the sidebar's foot.
    Account,
    Settings,
    SignIn,
    SignOut,
}

/// Which menu is open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    Tab,
    /// The focused pane's (a right click in it).
    Pane,
    /// The account button's, at the sidebar's foot: who's signed in,
    /// Settings, signing in or out. It opens upward from the point.
    Account,
}

/// A tab's menu, the focused pane's or the account's, open at a point.
struct TabMenu {
    tab: usize,
    x: f32,
    y: f32,
    kind: MenuKind,
    /// When it opened: it fades in from there.
    opened: Instant,
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
    /// How long ago its newest unseen finish came in, for the green to pop.
    done_for: f32,
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
    /// The built-in viewers, which sit in tabs' trees beside the panes.
    viewers: Vec<Viewer>,
    /// Where the viewers were last put.
    viewers_placed: Vec<(u64, [f32; 4], bool)>,
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
    /// Closes this window or quits, once the loop has ended and the GPU is
    /// let go: the main thread exits right after, and the Vulkan driver
    /// tearing itself down then mustn't race this core destroying its device.
    leave: Option<fn()>,
    /// When the frame on screen goes stale on its own (spinner, blink).
    animation_at: Option<Instant>,
    pointer: Pointer,
    /// Per frame: the tab bar's hit areas and the active tab's dividers.
    tab_hits: Vec<(Rect, usize)>,
    close_hits: Vec<(Rect, usize)>,
    new_tab_hit: Rect,
    collapse_hit: Rect,
    /// The account button at the sidebar's foot.
    account_hit: Rect,
    /// The extended sidebar's pane lines and the agent headers, by pane.
    pane_hits: Vec<(Rect, usize, u64)>,
    /// Each agent header, with its extension while open: a click there
    /// opens or closes it. And the one under the pointer.
    header_hits: Vec<(Rect, u64)>,
    header_hover: Option<u64>,
    menu_hits: Vec<(Rect, MenuAction)>,
    menu_rect: Rect,
    dividers: Vec<Divider>,
    tab_menu: Option<TabMenu>,
    renaming: Option<Rename>,
    /// The Settings text field being typed in, its text, and whether
    /// that's all selected.
    field: Option<(Field, String, bool)>,
    hover_tab: Option<usize>,
    /// When the pointer came onto the hovered tab, for its path's label.
    hover_at: Instant,
    mouse: (f32, f32),
    /// The modifier keys held, as last reported.
    held: u32,
    /// What an input method is composing, drawn at the cursor.
    preedit: String,
    /// The caret last reported to the platform, in window pixels.
    caret_sent: Option<[i32; 3]>,
    /// The pane installing a shell or a fetch from Settings ▸ Shell, and which.
    installing: Option<(u64, Install)>,
    /// The Settings tab's page, and what it shows that the theme doesn't.
    settings: settings::Page,
    /// The theme library, the scheme in use, and the background picked
    /// over it (None: the scheme's own).
    themes: Vec<Palette>,
    /// The keys file, as last read.
    bindings: Bindings,
    palette: usize,
    /// The theme the pointer is over in Settings' theme list, shown for now
    /// in place of `palette`.
    previewing: Option<usize>,
    background: Option<usize>,
    opacity: usize,
    blur: bool,
    /// Which of KINDS are installed, as of the Settings tab opening.
    installed: [bool; KINDS.len()],
    /// Which of fetch::FETCHES are installed, as of opening Settings.
    fetches_installed: [bool; FETCHES.len()],
    update: Option<UpdateOffer>,
    /// Per frame: the update pill and card's hit areas, and the card.
    update_hits: Vec<(Rect, UpdateAction)>,
    update_card: Rect,
    /// Phones asking to control this machine (device, name, code), and
    /// where their Allow and Deny buttons are.
    asks: Vec<(String, String, String)>,
    ask_hits: Vec<(Rect, String, bool)>,
    /// The toast on screen and when it came up: it fades in from there.
    toast_shown: Option<(String, Instant)>,
    /// Quitting to start the installed update.
    relaunch: bool,
    /// Whether this window keeps the tabs (and their output) for the next
    /// launch, and checks for updates: the first one does, and when it
    /// closes the oldest left takes over.
    primary: bool,
    /// `TEMINO_RECAP_TEST=<seconds>`: show the focused pane's recap this
    /// long after launch, as Command+I would, for screenshots.
    recap_test: Option<Instant>,
    /// `TEMINO_OPEN_TEST=<link>`: open it from the focused pane two
    /// seconds after launch, as a click on it would, for screenshots.
    open_test: Option<(Instant, String)>,
    /// `TEMINO_ASK_TEST=<request>`: ask it in AI mode from the focused pane
    /// two seconds after launch, as typing it and Enter would, for screenshots.
    ask_test: Option<(Instant, String)>,
    /// `TEMINO_TYPE_TEST=<command>|<command>…`: type each into the focused
    /// pane and press Enter, the first two seconds after launch and the rest
    /// a second apart, for screenshots. A last one left without `|` after it
    /// stays in the input, unsent.
    type_test: Option<(Instant, Vec<String>)>,
    /// When the state for remotes is next built, while any watch.
    remote_at: Instant,
}

/// How a window starts: with the tabs the last launch left (the first
/// window), or with one new tab in this directory (another window).
pub enum Start {
    Restore,
    Fresh(Option<String>),
}

/// The quick terminal's hotkey until the settings file names another.
const QUICK_TERMINAL_KEY: &str = "ctrl+`";

impl App {
    fn new(sender: Sender<Msg>, width: u32, height: u32, start: Start) -> Result<App, String> {
        let gpu = Gpu::new()?;
        let mut text = TextSystem::new(BASE_FONT_SIZE);
        text.ligatures = crate::setting("ligatures").as_deref() != Some("off");
        crate::trace("app: fonts ready");
        let background = load_background();
        let opacity = load_opacity();
        let themes = theme::library(&themes_dir());
        let palette = crate::setting("theme").and_then(|name| themes.iter().position(|t| t.name == name)).unwrap_or(0);
        let theme = build_theme(&themes[palette], background, OPACITIES[opacity] as f32 / 100.0);
        platform::set_background(theme.background.to_rgb8());
        platform::set_background_menu(&BACKGROUNDS.map(|(name, _)| name), background.unwrap_or(usize::MAX));
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
            viewers: Vec::new(),
            viewers_placed: Vec::new(),
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
            leave: None,
            animation_at: None,
            pointer: Pointer::Idle,
            tab_hits: Vec::new(),
            close_hits: Vec::new(),
            new_tab_hit: Rect::default(),
            collapse_hit: Rect::default(),
            account_hit: Rect::default(),
            pane_hits: Vec::new(),
            header_hits: Vec::new(),
            header_hover: None,
            menu_hits: Vec::new(),
            menu_rect: Rect::default(),
            dividers: Vec::new(),
            tab_menu: None,
            renaming: None,
            field: None,
            hover_tab: None,
            hover_at: Instant::now(),
            mouse: (0.0, 0.0),
            held: 0,
            preedit: String::new(),
            caret_sent: None,
            installing: None,
            settings: settings::Page::default(),
            themes,
            bindings: Bindings::default(),
            palette,
            previewing: None,
            background,
            opacity,
            blur: crate::setting("blur").as_deref() == Some("on"),
            installed: [false; KINDS.len()],
            fetches_installed: [false; FETCHES.len()],
            update: None,
            update_hits: Vec::new(),
            update_card: Rect::default(),
            asks: Vec::new(),
            ask_hits: Vec::new(),
            toast_shown: None,
            remote_at: Instant::now(),
            open_test: std::env::var("TEMINO_OPEN_TEST")
                .ok()
                .map(|link| (Instant::now() + Duration::from_secs(2), link)),
            ask_test: std::env::var("TEMINO_ASK_TEST")
                .ok()
                .map(|request| (Instant::now() + Duration::from_secs(2), request)),
            type_test: std::env::var("TEMINO_TYPE_TEST").ok().map(|commands| {
                (Instant::now() + Duration::from_secs(2), commands.split('|').rev().map(str::to_owned).collect())
            }),
            recap_test: std::env::var("TEMINO_RECAP_TEST")
                .ok()
                .and_then(|seconds| seconds.parse::<f32>().ok())
                .map(|seconds| Instant::now() + Duration::from_secs_f32(seconds)),
            relaunch: false,
            primary: matches!(start, Start::Restore),
        };
        app.apply_blur();
        app.bindings.refresh();
        app.apply_quick_terminal();
        app.set_zoom(
            crate::setting("zoom").and_then(|z| z.parse().ok()).unwrap_or(DEFAULT_FONT_SIZE / BASE_FONT_SIZE),
            false,
        );
        match start {
            Start::Restore => app.restore_tabs()?,
            Start::Fresh(cwd) => app.new_tab_in(cwd)?,
        }
        if app.primary {
            crate::update::check(app.ctx.sender.clone(), false);
        }
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
        self.new_tab_in(None)
    }

    /// A new tab in `cwd`, or else the focused pane's directory.
    fn new_tab_in(&mut self, cwd: Option<String>) -> Result<(), String> {
        let id = self.make_pane_in(cwd)?;
        // Settings ▸ Shell's fetch greets each new tab.
        if let Some(command) = fetch::command() {
            if let Some(pane) = self.pane(id) {
                pane.run_when_ready(command);
            }
        }
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
            ai_title: AiTitle::default(),
        });
        self.switch_tab(self.tabs.len() - 1);
        self.save_tabs();
        Ok(())
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
            Msg::Viewer(id, event) => self.viewer_event(id, event),
            Msg::Remote(command) => self.remote_command(command),
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
        if remote::watched() && now >= self.remote_at {
            self.remote_at = now + REMOTE_EVERY;
            let (state, lines) = self.remote_state();
            remote::publish(platform::window_no(), state, lines);
        }
        if self.open_test.as_ref().is_some_and(|(at, _)| now >= *at) {
            let (_, link) = self.open_test.take().unwrap();
            if let Some(id) = self.focused_id() {
                self.open_link(id, link);
            }
            self.dirty = true;
        }
        if self.type_test.as_ref().is_some_and(|(at, _)| now >= *at) {
            let (_, mut commands) = self.type_test.take().unwrap();
            if let Some(command) = commands.pop() {
                self.type_text(command.as_bytes());
                if !commands.is_empty() {
                    self.type_text(b"\r");
                    self.type_test = Some((now + Duration::from_secs(1), commands));
                }
            }
            self.dirty = true;
        }
        if self.ask_test.as_ref().is_some_and(|(at, _)| now >= *at) {
            let (_, request) = self.ask_test.take().unwrap();
            let id = self.focused_id();
            if let Some(pane) = self.panes.iter_mut().find(|p| Some(p.id) == id) {
                pane.ask_test(&mut self.ctx, &request);
            }
            self.dirty = true;
        }
        if self.recap_test.is_some_and(|at| now >= at) {
            self.recap_test = None;
            // With TEMINO_MENU_TEST=account, the account menu instead; with
            // tab or pane, that menu; with settings, the Settings tab.
            let menu = std::env::var("TEMINO_MENU_TEST").unwrap_or_default();
            let kind = match menu.as_str() {
                "account" if self.account_hit.w > 0.0 => {
                    Some((MenuKind::Account, self.account_hit.x, self.account_hit.y - 6.0))
                }
                "tab" => self.tab_hits.first().map(|(row, _)| (MenuKind::Tab, row.x + 40.0, row.bottom() - 6.0)),
                "pane" => Some((MenuKind::Pane, self.width * 0.5, self.height * 0.3)),
                _ => None,
            };
            if let Some((kind, x, y)) = kind {
                self.tab_menu = Some(TabMenu { tab: self.active, x, y, kind, opened: now });
                self.dirty = true;
                return;
            }
            if menu == "settings" {
                self.open_settings();
                self.dirty = true;
                return;
            }
            if let Some(id) = self.focused_id() {
                // With TEMINO_FIND_TEST=<query>, the search bar instead.
                let find = std::env::var("TEMINO_FIND_TEST").ok();
                let note = self.mark_note(id, now);
                if let Some(pane) = self.pane(id) {
                    match find {
                        Some(query) => pane.find_test(&query),
                        None => pane.focus_gained(now, None, note),
                    }
                }
            }
            self.dirty = true;
        }
    }

    fn next_wakeup(&self) -> Option<Instant> {
        let mut times: Vec<Option<Instant>> = self.panes.iter().map(Pane::next_wakeup).collect();
        times.push(self.animation_at);
        times.push(self.recap_test);
        times.push(self.open_test.as_ref().map(|(at, _)| *at));
        times.push(self.ask_test.as_ref().map(|(at, _)| *at));
        times.push(self.type_test.as_ref().map(|(at, _)| *at));
        times.push(self.ctx.toast.as_ref().map(|(_, at)| *at));
        times.push(remote::watched().then_some(self.remote_at));
        times.into_iter().flatten().min()
    }

    fn update_title(&mut self) {
        let Some(id) = self.focused_id() else { return };
        let title = match self.panes.iter().find(|p| p.id == id) {
            Some(pane) => format!("{} — Temino", pane.title()),
            None if id == SETTINGS => "Settings — Temino".to_owned(),
            None => match self.viewers.iter().find(|v| v.id == id) {
                Some(viewer) => format!("{} — Temino", viewer.title),
                None => return,
            },
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

    /// Quits Temino: this window and every other one.
    fn quit(&mut self) {
        self.shut_down();
        update::finish(self.relaunch);
        self.leave = Some(platform::quit);
    }

    /// Closes this window; the last one quits.
    fn close_window(&mut self) {
        if platform::window_count() <= 1 {
            self.quit();
            return;
        }
        self.shut_down();
        self.leave = Some(platform::close_window);
    }

    /// Done with this window: its tabs saved (if it keeps them) and its
    /// shells hung up; the loop then ends.
    fn shut_down(&mut self) {
        self.save_tabs();
        self.save_sessions();
        for pane in &mut self.panes {
            pane.hangup();
        }
        self.quitting = true;
    }

    // ---- events -------------------------------------------------------

    // ---- drawing ------------------------------------------------------
}

#[cfg(test)]
mod tests {
    use super::title_and_summary;

    #[test]
    fn reads_the_title_and_summary_lines() {
        let (title, summary) = title_and_summary("Temino tab colors\nTuning the tab palette; PR open.\n").unwrap();
        assert_eq!(title, "Temino tab colors");
        assert_eq!(summary.as_deref(), Some("Tuning the tab palette; PR open."));
        // Labels and numbering the model adds anyway come off.
        let (title, summary) = title_and_summary("1. Title: \"API logs\"\n\n2. Summary: Tailing the deploy").unwrap();
        assert_eq!(title, "API logs");
        assert_eq!(summary.as_deref(), Some("Tailing the deploy"));
        // One line is a title alone; nothing, or a paragraph, isn't one.
        assert_eq!(title_and_summary("Haiku VM build"), Some(("Haiku VM build".to_owned(), None)));
        assert_eq!(title_and_summary("  \n"), None);
        assert_eq!(title_and_summary(&"word ".repeat(20)), None);
    }
}

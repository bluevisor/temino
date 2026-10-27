//! The core thread: owns the renderer and the window's tabs, each a tree
//! of split panes; routes window events and pty output to panes, and draws
//! frames on demand.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::complete::Commands;
use crate::editor::History;
use crate::gpu::Gpu;
use crate::keys::Named;
use crate::pane::{Ctx, Pane};
use crate::platform::{self, buttons, Cursor, KeyEvent, UiEvent};
use crate::scene::{Quad, Rect, Scene, LAYER_CONTENT, LAYER_OVERLAY};
use crate::settings::{self, Action};
use crate::shells::{self, KINDS};
use crate::text::{Style, TextSystem, UI, UI_BOLD};
use crate::theme::{Color, Theme, BACKGROUNDS, OPACITIES, SURFACE_LIFT};
use crate::Msg;

/// The terminal font size at zoom 1, which every UI size is designed at.
const BASE_FONT_SIZE: f32 = 14.0;
/// The terminal font size at 100%: the window draws a little larger than
/// its design size.
const DEFAULT_FONT_SIZE: f32 = 15.4;
const TAB_BAR_HEIGHT: f32 = 34.0;
const TAB_MAX_WIDTH: f32 = 220.0;
const SIDEBAR_WIDTH: f32 = 200.0;
const SIDEBAR_ROW: f32 = 30.0;

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
const MENU_WIDTH: f32 = 200.0;
const MENU_ROW: f32 = 26.0;

/// The colors a tab can be marked with, from its menu.
const TAB_COLORS: [u32; 6] = [0xf2737a, 0xf0a35e, 0xe5c76b, 0x8ccf7e, 0x67b0e8, 0xc47fd5];

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
}

/// What a row of the tab menu does.
#[derive(Clone, Copy)]
enum MenuAction {
    Rename,
    Color(Option<usize>),
    Close,
    CloseOthers,
}

/// A tab's menu, open at a point.
struct TabMenu {
    tab: usize,
    x: f32,
    y: f32,
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
}

enum Pointer {
    Idle,
    /// A press went to this pane; drags and the release follow it.
    Pane(u64),
    /// Resizing the split at this path.
    Divider(Divider),
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
    strip: TabStrip,
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
    more_hits: Vec<(Rect, usize)>,
    menu_hits: Vec<(Rect, MenuAction)>,
    menu_rect: Rect,
    dividers: Vec<Divider>,
    tab_menu: Option<TabMenu>,
    renaming: Option<Rename>,
    hover_tab: Option<usize>,
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
        text.draw_mono(&mut scene, "ls -la ~/config ─┼─ ✓ 中文 🌞🐱", 32.0, 70.0, Style::default(), theme.success);
        scene.fill(Rect::new(32.0, 110.0, 120.0, 18.0), theme.accent);
        match gpu.render(&scene, &mut text.atlas, true) {
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
            strip: match std::env::var("TEMINO_TABS").ok().or_else(|| crate::setting("tabs")).as_deref() {
                Some("top") => TabStrip::Top,
                _ => TabStrip::Left,
            },
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
            more_hits: Vec::new(),
            menu_hits: Vec::new(),
            menu_rect: Rect::default(),
            dividers: Vec::new(),
            tab_menu: None,
            renaming: None,
            hover_tab: None,
            mouse: (0.0, 0.0),
            preedit: String::new(),
            caret_sent: None,
            installing: None,
            settings: settings::Page::default(),
            background,
            opacity,
            blur: crate::setting("blur").as_deref() == Some("on"),
            installed: [false; KINDS.len()],
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
        self.tabs.push(Tab { root: Node::Leaf(id), focus: id, zoomed: false, name: None, color: None });
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
            // active?, color, panes, name — separated by tabs.
            let mut fields = line.splitn(4, '\t');
            let (Some(current), Some(color), Some(layout), name) =
                (fields.next(), fields.next(), fields.next(), fields.next())
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
                format!("{current}\t{color}\t{layout}\t{name}\n")
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
        self.ctx.blink_epoch = Instant::now();
        let window = self.focused;
        if let Some(pane) = before.and_then(|id| self.panes.iter().find(|p| p.id == id)) {
            pane.focus_changed(false);
        }
        if let Some(pane) = after.and_then(|id| self.panes.iter().find(|p| p.id == id)) {
            pane.focus_changed(window);
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

    /// The tab list's area; empty while it is hidden.
    fn tab_area(&self) -> Rect {
        if !self.tab_list_visible() {
            return Rect::default();
        }
        match self.strip {
            TabStrip::Left => Rect::new(0.0, 0.0, SIDEBAR_WIDTH.min(self.width / 2.0), self.height),
            TabStrip::Top => Rect::new(0.0, 0.0, self.width, TAB_BAR_HEIGHT),
        }
    }

    fn content_rect(&self) -> Rect {
        let (width, height) = (self.width, self.height);
        let area = self.tab_area();
        match self.strip {
            TabStrip::Left => Rect::new(area.w, 0.0, (width - area.w).max(1.0), height),
            TabStrip::Top => Rect::new(0.0, area.h, width, (height - area.h).max(1.0)),
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
            Msg::Notice(message) => {
                self.ctx.toast = Some((message, Instant::now() + Duration::from_secs(6)));
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
    }

    fn next_wakeup(&self) -> Option<Instant> {
        let mut times: Vec<Option<Instant>> = self.panes.iter().map(Pane::next_wakeup).collect();
        times.push(self.animation_at);
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
        crate::update::apply_at_quit();
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
                self.focused = focused;
                self.ctx.blink_epoch = Instant::now();
                if let Some(id) = self.focused_id() {
                    if let Some(pane) = self.panes.iter().find(|p| p.id == id) {
                        pane.focus_changed(focused);
                    }
                }
            }
            UiEvent::Close => self.quit(),
            UiEvent::Text(text) => {
                self.preedit.clear();
                self.ctx.blink_epoch = Instant::now();
                if let Some(id) = self.focused_id() {
                    if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
                        pane.text_input(&text);
                    }
                }
            }
            UiEvent::Preedit(text) => self.preedit = text,
            UiEvent::Background(index) => self.set_background(index),
            UiEvent::Shell(index) => self.set_shell(index),
            UiEvent::Wheel { dy, x, y, .. } => {
                let (x, y) = (x / self.zoom, y / self.zoom);
                if let Some((SETTINGS, _)) = self.pane_at(x, y) {
                    self.settings.wheel(dy);
                } else if let Some((id, rect)) = self.pane_at(x, y) {
                    let Self { panes, ctx, .. } = self;
                    if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                        pane.wheel(ctx, dy, x - rect.x, y - rect.y);
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
        if self.rename_key(&key) {
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
                    self.ctx.show_toast("Checking for updates…");
                    crate::update::check(self.ctx.sender.clone(), true);
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
        self.tabs.push(Tab { root: Node::Leaf(SETTINGS), focus: SETTINGS, zoomed: false, name: None, color: None });
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
                self.ctx.show_toast("Checking for updates…");
                crate::update::check(self.ctx.sender.clone(), true);
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
    fn move_tab(&mut self, from: usize, to: usize) {
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
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
            let name = rename.name.trim();
            tab.name = (!name.is_empty()).then(|| name.to_owned());
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
        self.tab_menu = Some(TabMenu { tab, x, y });
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
            MenuAction::Close => self.close_tab(tab),
            MenuAction::CloseOthers => {
                for index in (0..self.tabs.len()).rev().filter(|&i| i != tab) {
                    self.close_tab(index);
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
        if let Some(menu) = self.tab_menu.take() {
            // A click anywhere else only closes the menu.
            if let Some(&(_, action)) = self.menu_hits.iter().find(|(r, _)| r.contains(x, y)) {
                self.menu_action(menu.tab, action);
            } else if self.menu_rect.contains(x, y) {
                self.tab_menu = Some(menu);
            }
            return;
        }
        let on_renamed = self
            .renaming
            .as_ref()
            .is_some_and(|rename| self.tab_hits.iter().any(|(r, i)| *i == rename.tab && r.contains(x, y)));
        if !on_renamed {
            self.finish_rename();
        }
        if self.tab_area().contains(x, y) {
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
        self.pointer = Pointer::Pane(id);
        let Self { panes, ctx, .. } = self;
        if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
            pane.mouse_down(ctx, x - rect.x, y - rect.y, pressed, clicks, mods);
        }
    }

    fn mouse_moved(&mut self, x: f32, y: f32, pressed: i32) {
        self.mouse = (x, y);
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
        self.hover_tab = self.tab_hits.iter().find(|(r, _)| r.contains(x, y)).map(|&(_, index)| index);
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
        if self.tab_area().contains(x, y) {
            self.set_cursor(Cursor::Arrow);
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
        for divider in &self.dividers {
            self.scene.set_layer(LAYER_OVERLAY);
            self.scene.fill(divider.line, self.ctx.theme.divider.mix(self.ctx.theme.text_faint, 0.35));
            self.scene.set_layer(LAYER_CONTENT);
        }
        self.draw_tab_bar();
        self.draw_tab_menu();
        self.draw_toast();
        self.draw_preedit();
        self.report_caret();
        crate::trace("render: scene built");
        let capture = std::env::var_os("TEMINO_CAPTURE");
        let on_screen = self.gpu.on_screen();
        match self.gpu.render(&self.scene, &mut self.ctx.text.atlas, capture.is_some()) {
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
        tab.name
            .clone()
            .unwrap_or_else(|| self.panes.iter().find(|p| p.id == tab.focus).map(Pane::title).unwrap_or_default())
    }

    fn draw_tab_bar(&mut self) {
        self.tab_hits.clear();
        self.close_hits.clear();
        self.more_hits.clear();
        self.new_tab_hit = Rect::default();
        if !self.tab_list_visible() {
            return;
        }
        let tabs: Vec<TabView> = (0..self.tabs.len())
            .map(|index| {
                let mut leaves = Vec::new();
                self.tabs[index].root.leaves(&mut leaves);
                TabView {
                    title: self.tab_title(index),
                    panes: leaves.len(),
                    color: self.tabs[index].color.map(|c| Color::hex(TAB_COLORS[c])),
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

    /// One tab's row: number, color dot, title (or the rename field), the
    /// ⋯ menu button while hovered or active, an optional badge, and ×;
    /// records the hit areas.
    fn draw_tab(&mut self, index: usize, tab: &TabView, rect: Rect, badge: Option<String>) {
        let current = index == self.active;
        let hovered = self.hover_tab == Some(index);
        // The sidebar marks colors with its bar; the strip with a dot.
        let dot = tab.color.filter(|_| self.strip == TabStrip::Top);
        let Self { scene, ctx, tab_hits, close_hits, more_hits, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let ascent = text.ui_ascent(12.0);
        let baseline = (rect.y + (rect.h + ascent * 0.72) / 2.0).round();
        let mut x = rect.x + 12.0;
        if index < 9 {
            x += text.draw_text(scene, &format!("{}", index + 1), x, baseline, UI, 11.0, theme.text_faint) + 8.0;
        }
        if let Some(color) = dot {
            scene.quad(Quad::new(Rect::new(x, rect.y + (rect.h - 8.0) / 2.0, 8.0, 8.0), color).radius(4.0));
            x += 14.0;
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
        if let Some((name, selected)) = &tab.editing {
            let field = Rect::new(x - 4.0, rect.y + 4.0, (close.x - x).max(0.0), rect.h - 8.0);
            scene.quad(Quad::new(field, theme.background).radius(4.0).border(1.0, theme.accent));
            scene.push_clip(field.inset(2.0, 0.0));
            let w = text.measure(name, UI, 12.0);
            if *selected && !name.is_empty() {
                scene.quad(
                    Quad::new(Rect::new(x - 1.0, field.y + 3.0, w + 2.0, field.h - 6.0), theme.selection).radius(2.0),
                );
            }
            text.draw_text(scene, name, x, baseline, UI, 12.0, theme.text);
            scene.fill(Rect::new(x + w + 1.0, field.y + 4.0, 1.0, field.h - 8.0), theme.cursor);
            scene.pop_clip();
        } else {
            scene.push_clip(Rect::new(x, rect.y, (right - x).max(0.0), rect.h));
            let color = if current { theme.text } else { theme.text_dim };
            text.draw_text(scene, &tab.title, x, baseline, UI, 12.0, color);
            scene.pop_clip();
        }
        text.draw_text(scene, "×", close.x + 4.0, baseline, UI, 13.0, theme.text_faint);
        tab_hits.push((rect, index));
        close_hits.push((close, index));
    }

    /// The tabs as a strip across the top.
    fn draw_tab_strip(&mut self, tabs: &[TabView]) {
        let width = self.width;
        let theme = &self.ctx.theme;
        self.scene.fill(Rect::new(0.0, 0.0, width, TAB_BAR_HEIGHT), theme.panel(SURFACE_LIFT / 2.0));
        self.scene.fill(Rect::new(0.0, TAB_BAR_HEIGHT - 1.0, width, 1.0), theme.divider);
        let plus = 34.0;
        let tab_width = ((width - 12.0 - plus) / tabs.len() as f32).min(TAB_MAX_WIDTH);
        for (index, tab) in tabs.iter().enumerate() {
            let rect = Rect::new(6.0 + index as f32 * tab_width, 5.0, tab_width - 4.0, TAB_BAR_HEIGHT - 9.0);
            if index == self.active {
                let theme = &self.ctx.theme;
                self.scene.quad(Quad::new(rect, theme.raised).radius(6.0).border(1.0, theme.divider));
                if let Some(color) = tab.color {
                    let line = Rect::new(rect.x + 8.0, rect.bottom() - 2.0, rect.w - 16.0, 2.0);
                    self.scene.quad(Quad::new(line, color).radius(1.0));
                }
            }
            let badge = (tab.panes > 1).then(|| format!("{}", tab.panes));
            self.draw_tab(index, tab, rect, badge);
        }
        let Self { scene, ctx, new_tab_hit, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let px = 6.0 + tabs.len() as f32 * tab_width;
        let plus_rect = Rect::new(px, 5.0, 26.0, TAB_BAR_HEIGHT - 9.0);
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
        self.scene.fill(Rect::new(area.right() - 1.0, 0.0, 1.0, area.h), theme.divider);
        self.scene.push_clip(Rect::new(0.0, 0.0, area.w - 1.0, area.h));
        let dragged = match self.pointer {
            Pointer::Tab { index, dragging: true, .. } => Some(index),
            _ => None,
        };
        let mut y = 8.0;
        for (index, tab) in tabs.iter().enumerate() {
            let rect = Rect::new(6.0, y, area.w - 13.0, SIDEBAR_ROW);
            let theme = &self.ctx.theme;
            if index == self.active {
                let fill = if dragged == Some(index) { theme.raised.mix(theme.text, 0.06) } else { theme.raised };
                self.scene.quad(Quad::new(rect, fill).radius(6.0).border(1.0, theme.divider));
            }
            if index == self.active || tab.color.is_some() {
                let bar = Rect::new(rect.x, rect.y + 7.0, 3.0, rect.h - 14.0);
                self.scene.quad(Quad::new(bar, tab.color.unwrap_or(theme.accent)).radius(1.5));
            }
            let badge = (tab.panes > 1).then(|| format!("{}", tab.panes));
            self.draw_tab(index, tab, rect, badge);
            y += SIDEBAR_ROW + 4.0;
        }
        let Self { scene, ctx, new_tab_hit, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let plus = Rect::new(6.0, y, area.w - 13.0, SIDEBAR_ROW);
        let ascent = text.ui_ascent(12.0);
        let baseline = (plus.y + (plus.h + ascent * 0.72) / 2.0).round();
        let label = format!("+  New tab   {}+T", platform::COMMAND_NAME);
        text.draw_text(scene, &label, plus.x + 12.0, baseline, UI, 12.0, theme.text_faint);
        *new_tab_hit = plus;
        scene.pop_clip();
    }

    /// The open tab menu: Rename, a row of colors, Close.
    fn draw_tab_menu(&mut self) {
        self.menu_hits.clear();
        self.menu_rect = Rect::default();
        let Some(menu) = &self.tab_menu else { return };
        let Some(tab) = self.tabs.get(menu.tab) else { return };
        let selected = tab.color;
        let several = self.tabs.len() > 1;
        let rows = if several { 4.0 } else { 3.0 };
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
        row_y += MENU_ROW + 4.0;
        scene.fill(Rect::new(x + 10.0, row_y, MENU_WIDTH - 20.0, 1.0), theme.divider);
        row_y += 5.0;
        item(scene, "Close Tab", MenuAction::Close, row_y);
        if several {
            item(scene, "Close Other Tabs", MenuAction::CloseOthers, row_y + MENU_ROW);
        }
        menu_hits.extend(swatches);
        scene.set_layer(LAYER_CONTENT);
        *menu_rect = rect;
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

fn tabs_path() -> String {
    format!("{}/tabs", crate::settings_dir())
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

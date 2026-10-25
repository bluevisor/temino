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
use crate::text::{Style, TextSystem, UI, UI_BOLD};
use crate::theme::{Color, Theme};
use crate::Msg;

const DEFAULT_FONT_SIZE: f32 = 14.0;
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
}

enum Pointer {
    Idle,
    /// A press went to this pane; drags and the release follow it.
    Pane(u64),
    /// Resizing the split at this path.
    Divider(Divider),
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
    width: u32,
    height: u32,
    scale: f32,
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
    dividers: Vec<Divider>,
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
        scene.reset(width as f32, height as f32, 1.0, theme.background);
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
                    write_ppm(std::ffi::OsStr::new(path), pixels, width, height);
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

impl App {
    fn new(sender: Sender<Msg>, width: u32, height: u32) -> Result<App, String> {
        let gpu = Gpu::new()?;
        let text = TextSystem::new(DEFAULT_FONT_SIZE);
        crate::trace("app: fonts ready");
        let theme = Theme::dark();
        platform::set_background(theme.background.to_rgb8());
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
        };
        let mut app = App {
            gpu,
            scene: Scene::new(),
            ctx,
            panes: Vec::new(),
            tabs: Vec::new(),
            active: 0,
            strip: match std::env::var("TEMINO_TABS").as_deref() {
                Ok("top") => TabStrip::Top,
                _ => TabStrip::Left,
            },
            tab_list: None,
            next_id: 1,
            width,
            height,
            scale: 1.0,
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
            dividers: Vec::new(),
        };
        app.new_tab()?;
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
    fn make_pane(&mut self) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        let cwd = self.focused_id().and_then(|f| self.panes.iter().find(|p| p.id == f)).map(|p| p.session.cwd.clone());
        let rect = self.content_rect();
        // Only the window's first pane greets with the banner.
        let pane = Pane::new(id, &mut self.ctx, rect, cwd, id == 1)?;
        self.panes.push(pane);
        Ok(id)
    }

    fn new_tab(&mut self) -> Result<(), String> {
        let id = self.make_pane()?;
        self.tabs.push(Tab { root: Node::Leaf(id), focus: id, zoomed: false });
        self.switch_tab(self.tabs.len() - 1);
        Ok(())
    }

    fn split(&mut self, axis: Axis) {
        let Some(focus) = self.focused_id() else { return };
        match self.make_pane() {
            Ok(id) => {
                let tab = &mut self.tabs[self.active];
                tab.root.split(focus, id, axis);
                tab.zoomed = false;
                self.set_focus(id);
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
            TabStrip::Left => Rect::new(0.0, 0.0, SIDEBAR_WIDTH.min(self.width as f32 / 2.0), self.height as f32),
            TabStrip::Top => Rect::new(0.0, 0.0, self.width as f32, TAB_BAR_HEIGHT),
        }
    }

    fn content_rect(&self) -> Rect {
        let (width, height) = (self.width as f32, self.height as f32);
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
                if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                    pane.feed(ctx, &bytes);
                }
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
        let Some(pane) = self.panes.iter().find(|p| p.id == id) else { return };
        let title = format!("{} — Temino", pane.title());
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
        for pane in &mut self.panes {
            pane.hangup();
        }
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
                self.width = width;
                self.height = height;
                if scale != self.scale {
                    self.scale = scale;
                    self.ctx.text.set_scale(scale);
                    for pane in &mut self.panes {
                        pane.cells_changed(&self.ctx.text);
                    }
                }
                self.relayout();
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
            UiEvent::Wheel { dy, x, y, .. } => {
                if let Some((id, rect)) = self.pane_at(x, y) {
                    let Self { panes, ctx, .. } = self;
                    if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                        pane.wheel(ctx, dy, x - rect.x, y - rect.y);
                    }
                }
            }
            UiEvent::MouseDown { x, y, buttons, clicks, mods } => self.mouse_down(x, y, buttons, clicks, mods),
            UiEvent::MouseMoved { x, y, buttons } => self.mouse_moved(x, y, buttons),
            UiEvent::MouseUp { x, y } => self.mouse_up(x, y),
        }
    }

    fn change_font_size(&mut self, size: f32) {
        let size = size.clamp(8.0, 32.0);
        if size == self.ctx.text.cell.size {
            return;
        }
        self.ctx.text.set_size(size);
        for pane in &mut self.panes {
            pane.cells_changed(&self.ctx.text);
        }
        self.ctx.show_toast(&format!("Font size {size:.0}"));
    }

    /// Window shortcuts (tabs, splits, font size); the rest go to the pane.
    fn key(&mut self, key: KeyEvent) {
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
                    self.change_font_size(self.ctx.text.cell.size + 1.0);
                    true
                }
                (_, Some('-')) => {
                    self.change_font_size(self.ctx.text.cell.size - 1.0);
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

    fn pane_at(&self, x: f32, y: f32) -> Option<(u64, Rect)> {
        self.visible_panes().into_iter().find(|(_, rect)| rect.contains(x, y))
    }

    fn mouse_down(&mut self, x: f32, y: f32, pressed: i32, clicks: i32, mods: u32) {
        if self.tab_area().contains(x, y) {
            if let Some(&(_, index)) = self.close_hits.iter().find(|(r, _)| r.contains(x, y)) {
                self.close_tab(index);
            } else if let Some(&(_, index)) = self.tab_hits.iter().find(|(r, _)| r.contains(x, y)) {
                if pressed & buttons::TERTIARY != 0 {
                    self.close_tab(index);
                } else {
                    self.switch_tab(index);
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
        let Self { panes, ctx, .. } = self;
        let cursor = panes.iter_mut().find(|p| p.id == id).map(|pane| pane.mouse_moved(ctx, x - rect.x, y - rect.y, 0));
        if let Some(cursor) = cursor {
            self.set_cursor(cursor);
        }
    }

    fn mouse_up(&mut self, x: f32, y: f32) {
        if let Pointer::Pane(id) = std::mem::replace(&mut self.pointer, Pointer::Idle) {
            let Self { panes, ctx, .. } = self;
            if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                let (ox, oy) = (pane.rect.x, pane.rect.y);
                pane.mouse_up(ctx, x - ox, y - oy);
            }
        }
    }

    // ---- drawing ------------------------------------------------------

    fn render(&mut self) {
        crate::trace("render: begin");
        self.dirty = false;
        let (width, height) = (self.width as f32, self.height as f32);
        self.scene.reset(width, height, self.scale, self.ctx.theme.background);
        let focus = self.focused_id();
        let visible = self.visible_panes();
        let several = visible.len() > 1;
        let mut animation: Option<Instant> = None;
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
        self.draw_toast();
        crate::trace("render: scene built");
        match self.gpu.render(&self.scene, &mut self.ctx.text.atlas) {
            Ok(pixels) => {
                let (pw, ph) = self.scene.pixel_size();
                if let Some(path) = std::env::var_os("TEMINO_CAPTURE") {
                    write_ppm(&path, pixels, pw, ph);
                }
                platform::present(pixels, pw, ph)
            }
            Err(error) => {
                crate::fatal(&format!("rendering failed: {error}"));
                self.quit();
            }
        }
    }

    fn draw_tab_bar(&mut self) {
        self.tab_hits.clear();
        self.close_hits.clear();
        self.new_tab_hit = Rect::default();
        if !self.tab_list_visible() {
            return;
        }
        let width = self.width as f32;
        // (title, number of panes) per tab.
        let tabs: Vec<(String, usize)> = (0..self.tabs.len())
            .map(|index| {
                let id = self.tabs[index].focus;
                let mut leaves = Vec::new();
                self.tabs[index].root.leaves(&mut leaves);
                let title = self.panes.iter().find(|p| p.id == id).map(Pane::title).unwrap_or_default();
                (title, leaves.len())
            })
            .collect();
        if self.strip == TabStrip::Left {
            return self.draw_tab_sidebar(&tabs);
        }
        let titles: Vec<String> = tabs
            .into_iter()
            .map(|(title, panes)| if panes > 1 { format!("{title}  ·  {panes} panes") } else { title })
            .collect();
        let Self { scene, ctx, tab_hits, close_hits, new_tab_hit, active, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let bar = Rect::new(0.0, 0.0, width, TAB_BAR_HEIGHT);
        scene.fill(bar, theme.surface.mix(theme.background, 0.5));
        scene.fill(Rect::new(0.0, TAB_BAR_HEIGHT - 1.0, width, 1.0), theme.divider);
        let plus = 34.0;
        let tab_width = ((width - 12.0 - plus) / titles.len() as f32).min(TAB_MAX_WIDTH);
        let ascent = text.ui_ascent(12.0);
        for (index, title) in titles.iter().enumerate() {
            let rect = Rect::new(6.0 + index as f32 * tab_width, 5.0, tab_width - 4.0, TAB_BAR_HEIGHT - 9.0);
            let current = index == *active;
            if current {
                scene.quad(Quad::new(rect, theme.raised).radius(6.0).border(1.0, theme.divider));
            }
            let baseline = (rect.y + (rect.h + ascent * 0.72) / 2.0).round();
            let number = if index < 9 { format!("{}", index + 1) } else { String::new() };
            let mut x = rect.x + 10.0;
            if !number.is_empty() {
                x += text.draw_text(scene, &number, x, baseline, UI, 11.0, theme.text_faint) + 8.0;
            }
            let close = Rect::new(rect.right() - 22.0, rect.y + (rect.h - 16.0) / 2.0, 16.0, 16.0);
            scene.push_clip(Rect::new(x, rect.y, (close.x - 4.0 - x).max(0.0), rect.h));
            let color = if current { theme.text } else { theme.text_dim };
            text.draw_text(scene, title, x, baseline, UI, 12.0, color);
            scene.pop_clip();
            text.draw_text(scene, "×", close.x + 4.0, baseline, UI, 13.0, theme.text_faint);
            tab_hits.push((rect, index));
            close_hits.push((close, index));
        }
        let px = 6.0 + titles.len() as f32 * tab_width;
        let plus_rect = Rect::new(px, 5.0, 26.0, TAB_BAR_HEIGHT - 9.0);
        text.draw_text(
            scene,
            "+",
            px + 8.0,
            (plus_rect.y + (plus_rect.h + ascent) / 2.0).round(),
            UI,
            15.0,
            theme.text_dim,
        );
        *new_tab_hit = plus_rect;
    }

    /// The tabs as a list down the left side.
    fn draw_tab_sidebar(&mut self, tabs: &[(String, usize)]) {
        let area = self.tab_area();
        let Self { scene, ctx, tab_hits, close_hits, new_tab_hit, active, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.fill(area, theme.surface.mix(theme.background, 0.5));
        scene.fill(Rect::new(area.right() - 1.0, 0.0, 1.0, area.h), theme.divider);
        let ascent = text.ui_ascent(12.0);
        scene.push_clip(Rect::new(0.0, 0.0, area.w - 1.0, area.h));
        let mut y = 8.0;
        for (index, (title, panes)) in tabs.iter().enumerate() {
            let rect = Rect::new(6.0, y, area.w - 13.0, SIDEBAR_ROW);
            let current = index == *active;
            if current {
                scene.quad(Quad::new(rect, theme.raised).radius(6.0).border(1.0, theme.divider));
                scene.quad(Quad::new(Rect::new(rect.x, rect.y + 7.0, 3.0, rect.h - 14.0), theme.accent).radius(1.5));
            }
            let baseline = (rect.y + (rect.h + ascent * 0.72) / 2.0).round();
            let mut x = rect.x + 12.0;
            if index < 9 {
                x += text.draw_text(scene, &format!("{}", index + 1), x, baseline, UI, 11.0, theme.text_faint) + 8.0;
            }
            let close = Rect::new(rect.right() - 22.0, rect.y + (rect.h - 16.0) / 2.0, 16.0, 16.0);
            let mut right = close.x - 4.0;
            if *panes > 1 {
                // A small badge with the pane count.
                let label = format!("{panes}");
                let w = text.measure(&label, UI, 10.0) + 10.0;
                let badge = Rect::new(right - w, rect.y + (rect.h - 16.0) / 2.0, w, 16.0);
                scene.quad(Quad::new(badge, theme.divider).radius(8.0));
                text.draw_text(scene, &label, badge.x + 5.0, badge.y + 12.0, UI, 10.0, theme.text_dim);
                right = badge.x - 6.0;
            }
            scene.push_clip(Rect::new(x, rect.y, (right - x).max(0.0), rect.h));
            let color = if current { theme.text } else { theme.text_dim };
            text.draw_text(scene, title, x, baseline, UI, 12.0, color);
            scene.pop_clip();
            text.draw_text(scene, "×", close.x + 4.0, baseline, UI, 13.0, theme.text_faint);
            tab_hits.push((rect, index));
            close_hits.push((close, index));
            y += SIDEBAR_ROW + 4.0;
        }
        let plus = Rect::new(6.0, y, area.w - 13.0, SIDEBAR_ROW);
        let baseline = (plus.y + (plus.h + ascent * 0.72) / 2.0).round();
        let label = format!("+  New tab   {}+T", platform::COMMAND_NAME);
        text.draw_text(scene, &label, plus.x + 12.0, baseline, UI, 12.0, theme.text_faint);
        *new_tab_hit = plus;
        scene.pop_clip();
    }

    fn draw_toast(&mut self) {
        let Some((message, _)) = &self.ctx.toast else { return };
        let message = message.clone();
        let (right, bottom) = match self.focused_id().and_then(|id| self.panes.iter().find(|p| p.id == id)) {
            Some(pane) => (pane.rect.right(), pane.rect.y + pane.input_top()),
            None => (self.width as f32, self.height as f32),
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

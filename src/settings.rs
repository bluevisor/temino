//! The Settings tab (Command+,): every setting on one page, drawn like the
//! rest of the window. The page only draws and says what a click asks for;
//! app.rs applies it and remembers it in the settings file.

use crate::scene::{Quad, Rect, Scene, LAYER_CONTENT, LAYER_OVERLAY};
use crate::shells::KINDS;
use crate::text::{TextSystem, UI, UI_BOLD};
use crate::theme::{Color, Palette, Theme, BACKGROUNDS, OPACITIES};

/// The page's column, at most this wide, centered in the tab.
const COLUMN: f32 = 640.0;
const PAD: f32 = 32.0;
const ROW: f32 = 58.0;
const CONTROL_HEIGHT: f32 = 26.0;
const SWATCH: f32 = 22.0;
const FIELD_WIDTH: f32 = 260.0;
/// How far one wheel line scrolls the page.
const WHEEL_STEP: f32 = 48.0;
/// The theme list: one line per theme, at most this tall before it scrolls.
const LIST_ROW: f32 = 28.0;
const LIST_HEIGHT: f32 = 340.0;

/// What the settings are now, for drawing them.
pub struct Values {
    /// The background picked over the theme's own, if any.
    pub background: Option<usize>,
    /// The theme's name, and how many the library holds.
    pub theme: String,
    pub themes: usize,
    /// The whole library, and which of it is in use, for the theme list.
    pub palettes: Vec<Palette>,
    pub palette: usize,
    /// How many key bindings the keys file holds.
    pub bindings: usize,
    pub ligatures: bool,
    /// The quick terminal's hotkey, if it's on; the outer None where there
    /// is no quick terminal.
    pub quick_terminal: Option<Option<String>>,
    /// An index into OPACITIES.
    pub opacity: usize,
    pub blur: bool,
    /// Which of shells::KINDS new panes start, and which are installed.
    pub shell: Option<usize>,
    pub installed: [bool; KINDS.len()],
    pub tabs_top: bool,
    /// As a percentage of the default size.
    pub zoom: f32,
    pub updates: bool,
    /// The window can be see-through (macOS).
    pub see_through: bool,
    /// The header over a pane running an agent (or a listed program).
    pub headers: bool,
    /// What each text field holds now, in `Field::ALL` order.
    pub fields: [String; Field::ALL.len()],
    /// The field being typed in, its text, and whether that's all selected.
    pub editing: Option<(Field, String, bool)>,
}

/// A setting typed in rather than picked: the model server and the keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    LlmUrl,
    LlmKey,
    TypesafeKey,
}

impl Field {
    pub const ALL: [Field; 3] = [Field::LlmUrl, Field::LlmKey, Field::TypesafeKey];

    /// Its line in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Field::LlmUrl => "llm_url",
            Field::LlmKey => "llm_key",
            Field::TypesafeKey => "typesafe_key",
        }
    }

    /// Keys show as dots.
    fn secret(self) -> bool {
        self != Field::LlmUrl
    }

    fn index(self) -> usize {
        Field::ALL.iter().position(|&f| f == self).unwrap()
    }
}

/// What a click on the page asks for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    /// The theme this many places along the library.
    Theme(i32),
    /// Open or close the list of every theme.
    ThemeList,
    /// The theme at this place in the library, picked from the list.
    PickTheme(usize),
    OpenThemes,
    EditKeys,
    Ligatures(bool),
    QuickTerminal(bool),
    Background(usize),
    Opacity(usize),
    Blur(bool),
    Shell(usize),
    TabsTop(bool),
    /// Smaller (-1), the default (0) or bigger (+1).
    Zoom(i32),
    Updates(bool),
    CheckForUpdates,
    Headers(bool),
    /// Start typing in a text field.
    Edit(Field),
}

#[derive(Default)]
pub struct Page {
    /// Per frame: where each control is, in window coordinates.
    hits: Vec<(Rect, Action)>,
    scroll: f32,
    /// How tall the page was last drawn, and the tab it was drawn in.
    content_height: f32,
    view_height: f32,
    /// The theme list is open (a click on the theme's name), where it was
    /// last drawn, and how far it's scrolled.
    pub themes_open: bool,
    list: Rect,
    list_scroll: f32,
}

impl Page {
    /// The last drawn wins, so the theme list takes clicks over the rows under it.
    pub fn action_at(&self, x: f32, y: f32) -> Option<Action> {
        self.hits.iter().rev().find(|(rect, _)| rect.contains(x, y)).map(|&(_, action)| action)
    }

    /// Scrolls the theme list when the mouse is over it, the page otherwise.
    pub fn wheel(&mut self, dy: f32, x: f32, y: f32) {
        if self.themes_open && self.list.contains(x, y) {
            self.list_scroll += dy * LIST_ROW;
            return;
        }
        let most = (self.content_height - self.view_height).max(0.0);
        self.scroll = (self.scroll + dy * WHEEL_STEP).clamp(0.0, most);
    }

    /// Opens the theme list scrolled to the theme in use, or closes it.
    pub fn toggle_themes(&mut self) {
        self.themes_open = !self.themes_open;
        self.list_scroll = f32::NAN;
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        text: &mut TextSystem,
        theme: &Theme,
        rect: Rect,
        values: &Values,
        mouse: (f32, f32),
    ) {
        self.hits.clear();
        self.view_height = rect.h;
        let width = (rect.w - 2.0 * PAD).min(COLUMN);
        let x = rect.x + ((rect.w - width) / 2.0).max(PAD);
        let top = rect.y + PAD - self.scroll;
        scene.push_clip(rect);
        let mut ui = Ui { scene, text, theme, hits: &mut self.hits, mouse, clip: rect };

        let mut y = top;
        ui.text.draw_text(ui.scene, "Settings", x, y + 24.0, UI_BOLD, 22.0, theme.text);
        y += 52.0;

        let mut rows = vec![Row::Theme, Row::ThemeFolder, Row::Background];
        if values.see_through {
            rows.extend([Row::Opacity, Row::Blur]);
        }
        y = ui.section(x, y, width, "Appearance", &rows, values);
        y = ui.section(x, y, width, "Text", &[Row::Zoom, Row::Ligatures], values);
        y = ui.section(x, y, width, "Shell", &[Row::Shell], values);
        let mut keys = vec![Row::Keys];
        if values.quick_terminal.is_some() {
            keys.push(Row::QuickTerminal);
        }
        y = ui.section(x, y, width, "Keys", &keys, values);
        y = ui.section(x, y, width, "Tabs", &[Row::TabsTop, Row::Headers], values);
        let fields = Field::ALL.map(Row::Field);
        y = ui.section(x, y, width, "AI", &fields, values);
        y = ui.section(x, y, width, "Updates", &[Row::Updates, Row::CheckForUpdates], values);
        let version = format!("Temino {}", env!("CARGO_PKG_VERSION"));
        ui.text.draw_text(ui.scene, &version, x, y + 4.0, UI, 11.5, theme.text_faint);
        y += 24.0;

        let anchor = ui.hits.iter().find(|&&(_, action)| action == Action::ThemeList).map(|&(rect, _)| rect);
        match anchor {
            Some(anchor) if self.themes_open => {
                let (list, scroll) = ui.theme_list(anchor, values, self.list_scroll);
                (self.list, self.list_scroll) = (list, scroll);
            }
            _ => self.themes_open = false,
        }

        self.content_height = y + self.scroll - rect.y + PAD;
        scene.pop_clip();
    }
}

/// One row of the page: a label and a description on the left, its control
/// on the right.
#[derive(Clone, Copy)]
enum Row {
    Theme,
    ThemeFolder,
    Keys,
    Ligatures,
    QuickTerminal,
    Background,
    Opacity,
    Blur,
    Zoom,
    Shell,
    TabsTop,
    Updates,
    CheckForUpdates,
    Headers,
    Field(Field),
}

struct Ui<'a> {
    scene: &'a mut Scene,
    text: &'a mut TextSystem,
    theme: &'a Theme,
    hits: &'a mut Vec<(Rect, Action)>,
    mouse: (f32, f32),
    /// The visible part of the page: controls outside it can't be clicked.
    clip: Rect,
}

impl Ui<'_> {
    fn hovered(&self, rect: Rect) -> bool {
        rect.contains(self.mouse.0, self.mouse.1) && self.clip.contains(self.mouse.0, self.mouse.1)
    }

    fn hit(&mut self, rect: Rect, action: Action) {
        self.hits.push((rect, action));
    }

    /// A heading and a card of rows under it; returns where the next one goes.
    fn section(&mut self, x: f32, y: f32, width: f32, title: &str, rows: &[Row], values: &Values) -> f32 {
        let theme = self.theme;
        self.text.draw_text(self.scene, &title.to_uppercase(), x + 2.0, y + 12.0, UI_BOLD, 10.5, theme.text_faint);
        let card = Rect::new(x, y + 22.0, width, ROW * rows.len() as f32);
        self.scene.quad(Quad::new(card, theme.panel(0.04)).radius(10.0).border(1.0, theme.divider));
        for (i, &row) in rows.iter().enumerate() {
            let row_rect = Rect::new(card.x, card.y + i as f32 * ROW, card.w, ROW);
            if i > 0 {
                self.scene.fill(Rect::new(row_rect.x + 16.0, row_rect.y, row_rect.w - 32.0, 1.0), theme.divider);
            }
            self.row(row_rect, row, values);
        }
        card.bottom() + 28.0
    }

    fn row(&mut self, rect: Rect, row: Row, values: &Values) {
        let theme = self.theme;
        let (label, detail) = match row {
            Row::Theme => {
                ("Theme", format!("{} of {} · colors for the terminal and the window", values.theme, values.themes))
            }
            Row::Keys => (
                "Key bindings",
                match values.bindings {
                    0 => "The defaults; the keys file rebinds any of them".to_owned(),
                    1 => "1 custom binding, from the keys file".to_owned(),
                    n => format!("{n} custom bindings, from the keys file"),
                },
            ),
            Row::Ligatures => ("Ligatures", "Symbols like -> != >= drawn joined".to_owned()),
            Row::QuickTerminal => (
                "Quick terminal",
                match values.quick_terminal.clone().flatten() {
                    Some(keys) => {
                        format!("{keys} from any app brings Temino, and hides it · quick_terminal= in settings")
                    }
                    None => "A hotkey from any app brings Temino forward".to_owned(),
                },
            ),
            Row::ThemeFolder => ("Your themes", "Ghostty theme files dropped here join the list".to_owned()),
            Row::Background => {
                ("Background", values.background.map_or("The theme's own".to_owned(), |i| BACKGROUNDS[i].0.to_owned()))
            }
            Row::Opacity => ("Background opacity", "How much of the desktop shows through".to_owned()),
            Row::Blur => ("Blur behind the window", "Soften what shows through".to_owned()),
            Row::Zoom => ("Size", format!("{:.0}% · Command+plus and Command+minus too", values.zoom)),
            Row::Shell => ("Default shell", "For new tabs and panes; a missing one is installed".to_owned()),
            Row::Headers => ("Status header", "Over a pane running an agent: what it's doing, for how long".to_owned()),
            Row::TabsTop => ("Tab list", "Down the side, or across the top".to_owned()),
            Row::Updates => ("Check for updates daily", "At launch, from GitHub releases".to_owned()),
            Row::CheckForUpdates => ("Check now", "Offers a newer version if there is one".to_owned()),
            Row::Field(Field::LlmUrl) => ("Model server", "OpenAI-compatible, for AI mode and tab recaps".to_owned()),
            Row::Field(Field::LlmKey) => ("Model server key", "If the server asks for one".to_owned()),
            Row::Field(Field::TypesafeKey) => ("TypeSafe key", "Lets Jev decide: command or answer".to_owned()),
        };
        let left = rect.x + 16.0;
        self.text.draw_text(self.scene, label, left, rect.y + 25.0, UI, 13.0, theme.text);
        self.text.draw_text(self.scene, &detail, left, rect.y + 42.0, UI, 11.5, theme.text_dim);
        let right = rect.right() - 16.0;
        let control_y = rect.y + (ROW - CONTROL_HEIGHT) / 2.0;
        match row {
            Row::Theme => {
                let labels = ["‹".to_owned(), values.theme.clone(), "›".to_owned()];
                self.segments(right, control_y, &labels, None, |i| match i {
                    1 => Action::ThemeList,
                    _ => Action::Theme(i as i32 - 1),
                });
            }
            Row::Ligatures => self.toggle(right, rect.y, values.ligatures, Action::Ligatures(!values.ligatures)),
            Row::QuickTerminal => {
                let on = values.quick_terminal.as_ref().is_some_and(Option::is_some);
                self.toggle(right, rect.y, on, Action::QuickTerminal(!on));
            }
            Row::Keys => self.button(right, control_y, "Edit Key Bindings", Action::EditKeys),
            Row::ThemeFolder => self.button(right, control_y, "Open Themes Folder", Action::OpenThemes),
            Row::Background => self.swatches(right, rect.y + (ROW - SWATCH) / 2.0, values.background),
            Row::Opacity => {
                let labels: Vec<String> = OPACITIES.iter().map(|o| format!("{o}")).collect();
                self.segments(right, control_y, &labels, Some(values.opacity), Action::Opacity);
            }
            Row::Blur => self.toggle(right, rect.y, values.blur, Action::Blur(!values.blur)),
            Row::Zoom => {
                let labels = ["−".to_owned(), "Default".to_owned(), "+".to_owned()];
                self.segments(right, control_y, &labels, None, |i| Action::Zoom(i as i32 - 1));
            }
            Row::Shell => {
                let labels: Vec<String> = KINDS
                    .iter()
                    .zip(values.installed)
                    .map(
                        |(kind, installed)| {
                            if installed {
                                kind.name().to_owned()
                            } else {
                                format!("{} ↓", kind.name())
                            }
                        },
                    )
                    .collect();
                self.segments(right, control_y, &labels, values.shell, Action::Shell);
            }
            Row::TabsTop => {
                let labels = ["Side".to_owned(), "Top".to_owned()];
                self.segments(right, control_y, &labels, Some(values.tabs_top as usize), |i| Action::TabsTop(i == 1));
            }
            Row::Updates => self.toggle(right, rect.y, values.updates, Action::Updates(!values.updates)),
            Row::CheckForUpdates => self.button(right, control_y, "Check for Updates", Action::CheckForUpdates),
            Row::Headers => self.toggle(right, rect.y, values.headers, Action::Headers(!values.headers)),
            Row::Field(field) => self.field(right, control_y, field, values),
        }
    }

    /// A text field, right-aligned at `right`: the setting's value (keys as
    /// dots), or what's being typed with a caret.
    fn field(&mut self, right: f32, y: f32, field: Field, values: &Values) {
        let theme = self.theme;
        let size = 12.0;
        let rect = Rect::new(right - FIELD_WIDTH, y, FIELD_WIDTH, CONTROL_HEIGHT);
        let editing = values.editing.as_ref().filter(|(f, _, _)| *f == field);
        let border = if editing.is_some() {
            theme.accent
        } else if self.hovered(rect) {
            theme.text_faint
        } else {
            theme.divider
        };
        self.scene.quad(Quad::new(rect, Color::hex(0x000000).alpha(0.22)).radius(7.0).border(1.0, border));
        let shown = |text: &str| {
            if field.secret() && !text.is_empty() {
                "•".repeat(text.chars().count().min(24))
            } else {
                text.to_owned()
            }
        };
        let baseline = (rect.y + (rect.h + self.text.ui_ascent(size) * 0.72) / 2.0).round();
        let x = rect.x + 10.0;
        self.scene.push_clip(rect.inset(8.0, 0.0));
        match editing {
            Some((_, text, selected)) => {
                let text = shown(text);
                let w = self.text.measure(&text, UI, size);
                // Long text scrolls so the caret stays in view.
                let x = x - (w - (rect.w - 24.0)).max(0.0);
                if *selected && !text.is_empty() {
                    let mark = Rect::new(x - 1.0, rect.y + 5.0, w + 2.0, rect.h - 10.0);
                    self.scene.quad(Quad::new(mark, theme.selection).radius(2.0));
                }
                self.text.draw_text(self.scene, &text, x, baseline, UI, size, theme.text);
                self.scene.fill(Rect::new(x + w + 1.0, rect.y + 6.0, 1.0, rect.h - 12.0), theme.cursor);
            }
            None => {
                let value = &values.fields[field.index()];
                let (text, color) = match (value.is_empty(), field) {
                    (true, Field::LlmUrl) => ("Orin: orin.local, then tailnet".to_owned(), theme.text_faint),
                    (true, _) => ("Not set".to_owned(), theme.text_faint),
                    (false, _) => (shown(value), theme.text),
                };
                self.text.draw_text(self.scene, &text, x, baseline, UI, size, color);
            }
        }
        self.scene.pop_clip();
        self.hit(rect, Action::Edit(field));
    }

    /// Every theme in a list dropped under `anchor` (the theme's name), the
    /// one in use lit, each with its background and a few of its colors.
    /// `scroll` NaN starts it on the one in use; returns where the list is
    /// and the scroll kept in range.
    fn theme_list(&mut self, anchor: Rect, values: &Values, scroll: f32) -> (Rect, f32) {
        let theme = self.theme;
        let size = 12.5;
        let widest = values.palettes.iter().map(|p| self.text.measure(&p.name, UI, size)).fold(0.0, f32::max);
        let w = (widest + 96.0).max(220.0);
        let content = LIST_ROW * values.palettes.len() as f32;
        let h = content.min(LIST_HEIGHT) + 8.0;
        let below = self.clip.bottom() - anchor.bottom() - 12.0;
        let y = if below >= h || below >= anchor.y - self.clip.y { anchor.bottom() + 6.0 } else { anchor.y - 6.0 - h };
        let x = (anchor.x + anchor.w / 2.0 - w / 2.0).min(self.clip.right() - w - 12.0).max(self.clip.x + 12.0);
        let list = Rect::new(x, y, w, h);
        let most = (content - (h - 8.0)).max(0.0);
        let scroll =
            if scroll.is_nan() { LIST_ROW * values.palette as f32 - (h - 8.0 - LIST_ROW) / 2.0 } else { scroll };
        let scroll = scroll.clamp(0.0, most);

        self.scene.set_layer(LAYER_OVERLAY);
        let shadow = Rect::new(list.x - 1.0, list.y + 5.0, list.w + 2.0, list.h + 2.0);
        self.scene.quad(Quad::new(shadow, Color::hex(0x000000).alpha(0.28)).radius(12.0));
        self.scene.quad(Quad::new(list, theme.raised).radius(10.0).border(1.0, theme.divider));
        // The list's padding takes clicks too, rather than the rows under it.
        self.hit(list, Action::PickTheme(values.palette));
        let inner = list.inset(4.0, 4.0);
        self.scene.push_clip(inner);
        let ascent = self.text.ui_ascent(size);
        for (index, palette) in values.palettes.iter().enumerate() {
            let line = Rect::new(inner.x, inner.y + index as f32 * LIST_ROW - scroll, inner.w, LIST_ROW);
            if line.bottom() < inner.y || line.y > inner.bottom() {
                continue;
            }
            let on = index == values.palette;
            if on {
                self.scene.quad(Quad::new(line, theme.accent.alpha(0.85)).radius(6.0));
            } else if self.hovered(line) && inner.contains(self.mouse.0, self.mouse.1) {
                self.scene.quad(Quad::new(line, Color::hex(0xffffff).alpha(0.07)).radius(6.0));
            }
            let chip = Rect::new(line.x + 8.0, line.y + 6.0, 44.0, LIST_ROW - 12.0);
            let chip_quad = Quad::new(chip, Color::hex(palette.background)).radius(4.0);
            self.scene.quad(chip_quad.border(1.0, Color::hex(0xffffff).alpha(0.18)));
            for (i, &ansi) in [1, 2, 4, 5].iter().enumerate() {
                let dot = Rect::new(chip.x + 5.0 + i as f32 * 9.0, chip.y + chip.h / 2.0 - 3.0, 6.0, 6.0);
                self.scene.quad(Quad::new(dot, Color::hex(palette.ansi[ansi])).radius(3.0));
            }
            let baseline = (line.y + (line.h + ascent * 0.72) / 2.0).round();
            let color = if on { Color::hex(0x0b0d12) } else { theme.text };
            self.text.draw_text(self.scene, &palette.name, chip.right() + 12.0, baseline, UI, size, color);
            let (top, bottom) = (line.y.max(inner.y), line.bottom().min(inner.bottom()));
            self.hit(Rect::new(line.x, top, line.w, bottom - top), Action::PickTheme(index));
        }
        self.scene.pop_clip();
        self.scene.set_layer(LAYER_CONTENT);
        (list, scroll)
    }

    /// The backgrounds as color dots, right-aligned at `right`.
    fn swatches(&mut self, right: f32, y: f32, selected: Option<usize>) {
        let theme = self.theme;
        let step = SWATCH + 10.0;
        let mut x = right - step * BACKGROUNDS.len() as f32 + 10.0;
        for (index, &(_, rgb)) in BACKGROUNDS.iter().enumerate() {
            let dot = Rect::new(x, y, SWATCH, SWATCH);
            let hit = dot.inset(-4.0, -4.0);
            let ring = if Some(index) == selected {
                theme.accent
            } else if self.hovered(hit) {
                theme.text_dim
            } else {
                Color::hex(0xffffff).alpha(0.18)
            };
            if Some(index) == selected {
                self.scene.quad(Quad::new(dot.inset(-3.0, -3.0), Color::TRANSPARENT).radius(SWATCH).border(2.0, ring));
                self.scene.quad(
                    Quad::new(dot, Color::hex(rgb)).radius(SWATCH / 2.0).border(1.0, Color::hex(0xffffff).alpha(0.18)),
                );
            } else {
                self.scene.quad(Quad::new(dot, Color::hex(rgb)).radius(SWATCH / 2.0).border(1.0, ring));
            }
            self.hit(hit, Action::Background(index));
            x += step;
        }
    }

    /// A row of joined buttons, right-aligned at `right`, `selected` lit.
    fn segments(
        &mut self,
        right: f32,
        y: f32,
        labels: &[String],
        selected: Option<usize>,
        action: impl Fn(usize) -> Action,
    ) {
        let theme = self.theme;
        let size = 12.0;
        let widths: Vec<f32> = labels.iter().map(|l| (self.text.measure(l, UI, size) + 20.0).max(34.0)).collect();
        let total: f32 = widths.iter().sum();
        let outer = Rect::new(right - total - 4.0, y, total + 4.0, CONTROL_HEIGHT);
        self.scene.quad(Quad::new(outer, Color::hex(0x000000).alpha(0.22)).radius(7.0).border(1.0, theme.divider));
        let ascent = self.text.ui_ascent(size);
        let mut x = outer.x + 2.0;
        for (index, (label, w)) in labels.iter().zip(widths).enumerate() {
            let segment = Rect::new(x, y + 2.0, w, CONTROL_HEIGHT - 4.0);
            let on = selected == Some(index);
            if on {
                self.scene.quad(Quad::new(segment, theme.accent.alpha(0.85)).radius(5.0));
            } else if self.hovered(segment) {
                self.scene.quad(Quad::new(segment, Color::hex(0xffffff).alpha(0.07)).radius(5.0));
            }
            let label_width = self.text.measure(label, UI, size);
            let baseline = (segment.y + (segment.h + ascent * 0.72) / 2.0).round();
            let color = if on { Color::hex(0x0b0d12) } else { theme.text };
            self.text.draw_text(self.scene, label, segment.x + (w - label_width) / 2.0, baseline, UI, size, color);
            self.hit(segment, action(index));
            x += w;
        }
    }

    /// An on/off switch, right-aligned at `right` and centered in the row at `row_y`.
    fn toggle(&mut self, right: f32, row_y: f32, on: bool, action: Action) {
        let theme = self.theme;
        let (w, h) = (38.0, 22.0);
        let track = Rect::new(right - w, row_y + (ROW - h) / 2.0, w, h);
        let fill =
            if on { theme.accent } else { Color::hex(0xffffff).alpha(if self.hovered(track) { 0.18 } else { 0.12 }) };
        self.scene.quad(Quad::new(track, fill).radius(h / 2.0));
        let knob = h - 4.0;
        let kx = if on { track.right() - knob - 2.0 } else { track.x + 2.0 };
        self.scene.quad(Quad::new(Rect::new(kx, track.y + 2.0, knob, knob), Color::hex(0xf4f6fa)).radius(knob / 2.0));
        self.hit(track.inset(-6.0, -6.0), action);
    }

    fn button(&mut self, right: f32, y: f32, label: &str, action: Action) {
        let theme = self.theme;
        let size = 12.0;
        let w = self.text.measure(label, UI, size) + 24.0;
        let rect = Rect::new(right - w, y, w, CONTROL_HEIGHT);
        let fill = if self.hovered(rect) { theme.raised.mix(Color::hex(0xffffff), 0.06) } else { theme.raised };
        self.scene.quad(Quad::new(rect, fill).radius(7.0).border(1.0, theme.divider));
        let baseline = (rect.y + (rect.h + self.text.ui_ascent(size) * 0.72) / 2.0).round();
        self.text.draw_text(self.scene, label, rect.x + 12.0, baseline, UI, size, theme.text);
        self.hit(rect, action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_can_be_clicked() {
        let mut scene = Scene::new();
        let mut text = TextSystem::new(14.0);
        let theme = Theme::dark();
        let values = Values {
            background: Some(0),
            theme: "Temino".into(),
            themes: 12,
            palettes: Vec::new(),
            palette: 0,
            bindings: 0,
            ligatures: true,
            quick_terminal: Some(None),
            opacity: 0,
            blur: false,
            shell: Some(0),
            installed: [true, true, false],
            tabs_top: false,
            zoom: 100.0,
            updates: true,
            see_through: true,
            headers: true,
            fields: Default::default(),
            editing: None,
        };
        let mut page = Page::default();
        let rect = Rect::new(200.0, 0.0, 900.0, 2000.0);
        scene.reset(1100.0, 2000.0, 1.0, theme.background);
        page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
        let actions: Vec<Action> = page.hits.iter().map(|&(_, a)| a).collect();
        let mut expected = vec![Action::Theme(-1), Action::ThemeList, Action::Theme(1), Action::OpenThemes];
        expected.extend((0..BACKGROUNDS.len()).map(Action::Background));
        expected.extend((0..OPACITIES.len()).map(Action::Opacity));
        expected.extend([
            Action::Blur(true),
            Action::Zoom(-1),
            Action::Zoom(0),
            Action::Zoom(1),
            Action::Ligatures(false),
        ]);
        expected.extend((0..KINDS.len()).map(Action::Shell));
        expected.extend([Action::EditKeys, Action::QuickTerminal(true)]);
        expected.extend([
            Action::TabsTop(false),
            Action::TabsTop(true),
            Action::Headers(false),
        ]);
        expected.extend(Field::ALL.map(Action::Edit));
        expected.extend([Action::Updates(false), Action::CheckForUpdates]);
        assert_eq!(actions, expected);
        // A click lands on what's under it, and nothing overlaps.
        for (rect, action) in &page.hits {
            let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            assert_eq!(page.action_at(x, y), Some(*action));
        }
    }

    #[test]
    fn the_page_scrolls_only_as_far_as_it_goes() {
        let mut page = Page { content_height: 900.0, view_height: 600.0, ..Page::default() };
        page.wheel(100.0, 0.0, 0.0);
        assert_eq!(page.scroll, 300.0);
        page.wheel(-100.0, 0.0, 0.0);
        assert_eq!(page.scroll, 0.0);
    }
}

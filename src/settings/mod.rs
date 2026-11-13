//! The Settings tab (Command+,): every setting, in sub tabs (Appearance,
//! Terminal, AI, …), drawn like the rest of the window. The page only draws and says what a click asks for;
//! app.rs applies it and remembers it in the settings file.

use crate::bindings::{Bind, Combo};
use crate::cursor::Shape;
use crate::fetch::FETCHES;
use crate::llm::{self, PROVIDERS};
use crate::scene::{Quad, Rect, Scene, LAYER_CONTENT, LAYER_OVERLAY};
use crate::shells::KINDS;
use crate::text::{Style, TextSystem, LINE_SPACINGS, UI, UI_BOLD};
use crate::theme::{backgrounds, Channel, Color, Mode, Palette, Slot, Theme, OPACITIES};

mod editor;
mod keys;
mod lists;
mod preview;
mod sections;
pub use editor::ThemeEdit;
pub use lists::List;
pub use preview::Preview;
pub use sections::Section;

/// The page's column, at most this wide, centered in the tab.
const COLUMN: f32 = 640.0;
const PAD: f32 = 32.0;
const ROW: f32 = 58.0;
const CONTROL_HEIGHT: f32 = 26.0;
const SWATCH: f32 = 22.0;
const FIELD_WIDTH: f32 = 260.0;
/// How far one wheel line scrolls the page.
const WHEEL_STEP: f32 = 48.0;
/// A key binding's row: one line, no description.
const KEY_ROW: f32 = 40.0;
/// The theme and font lists: one line each, at most this tall before they scroll.
const LIST_ROW: f32 = 28.0;
const LIST_HEIGHT: f32 = 340.0;
/// The Remote Control row that tells the phone's side, QR code and all.
const PHONE_ROW: f32 = 170.0;
/// The theme editor's rows of color dots, each named under it.
const COLORS_ROW: f32 = 74.0;
const QR_SIZE: f32 = 138.0;

/// What the settings are now, for drawing them.
pub struct Values {
    /// The background picked over the theme's own, if any.
    pub background: Option<usize>,
    /// The theme's name, and how many the library holds.
    pub theme: String,
    pub themes: usize,
    /// The whole library as it shows in this mode, and which of it is in
    /// use, for the theme list.
    pub palettes: Vec<Palette>,
    pub palette: usize,
    /// The theme editor, while it's open on a theme of your own.
    pub editor: Option<ThemeEdit>,
    /// Light or Dark: every theme in the one picked.
    pub mode: Mode,
    /// The keys file's bindings, and the action waiting for new keys.
    pub keys: Vec<(Combo, Bind)>,
    pub recording: Option<&'static str>,
    pub ligatures: bool,
    /// The terminal's font family; every one to pick from (while the
    /// font list is open), and which of them it is.
    pub font: String,
    pub fonts: Vec<String>,
    pub font_index: usize,
    /// The quick terminal's hotkey, if it's on; the outer None where there
    /// is no quick terminal.
    pub quick_terminal: Option<Option<String>>,
    /// An index into OPACITIES.
    pub opacity: usize,
    pub blur: bool,
    /// Which of shells::KINDS new panes start, and which are installed.
    pub shell: Option<usize>,
    pub installed: [bool; KINDS.len()],
    /// What new tabs open with: 0 nothing, else 1 + an index into FETCHES;
    /// and which of FETCHES are installed.
    pub fetch: usize,
    pub fetches_installed: [bool; FETCHES.len()],
    pub tabs_top: bool,
    /// The side tab list says, under the active tab, what each pane is doing.
    pub extended: bool,
    /// Desktop notifications for panes out of sight.
    pub notifications: bool,
    /// What panes ran at quit comes back at launch; dev servers too.
    pub resume_programs: bool,
    pub resume_servers: bool,
    /// As a percentage of the default size, and the terminal font's points.
    pub zoom: f32,
    pub points: f32,
    /// An index into LINE_SPACINGS.
    pub line_spacing: usize,
    /// An index into cursor::Shape::ALL, and whether it blinks.
    pub cursor: usize,
    pub blink: bool,
    pub updates: bool,
    /// Anonymous usage data is shared, with the user's yes (Settings ▸
    /// Share anonymous usage data; the permission card's answer too).
    pub analytics: bool,
    /// The window can be see-through (macOS).
    pub see_through: bool,
    /// Signed in for remote control; None hides the section (no relay).
    pub remote: Option<bool>,
    /// Who it's signed in as ("you@example.com (Google)", or a first name),
    /// once the relay has said.
    pub remote_account: String,
    /// The phone's web app, which the QR code under it opens.
    pub remote_url: String,
    /// The phones controlling this machine now, by name.
    pub remote_phones: Vec<String>,
    /// Where the model comes from: an index into llm::PROVIDERS.
    pub provider: usize,
    /// What each text field holds now, in `Field::ALL` order.
    pub fields: [String; Field::ALL.len()],
    /// The field being typed in, its text, and whether that's all selected.
    pub editing: Option<(Field, String, bool)>,
}

/// A setting typed in rather than picked: the model server, model and keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    LlmUrl,
    LlmKey,
    LlmModel,
    TypesafeKey,
    /// The theme editor's: the theme's name, and the picked color as hex.
    ThemeName,
    ThemeHex,
}

impl Field {
    pub const ALL: [Field; 4] = [Field::LlmUrl, Field::LlmKey, Field::LlmModel, Field::TypesafeKey];

    /// Its line in the settings file; the model's key and model are the
    /// chosen provider's own.
    pub fn key(self) -> String {
        match self {
            Field::LlmUrl => "llm_url".to_owned(),
            Field::LlmKey => llm::chosen().key_setting().to_owned(),
            Field::LlmModel => llm::chosen().model_setting(),
            Field::TypesafeKey => "typesafe_key".to_owned(),
            // Kept in the theme's file, not the settings file.
            Field::ThemeName | Field::ThemeHex => String::new(),
        }
    }

    /// Keys show as dots.
    fn secret(self) -> bool {
        matches!(self, Field::LlmKey | Field::TypesafeKey)
    }

    /// Where it is in Field::ALL (and Values::fields): the theme editor's
    /// two aren't there.
    fn index(self) -> Option<usize> {
        Field::ALL.iter().position(|&f| f == self)
    }
}

/// What a click on the page asks for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    /// The theme this many places along the library.
    Theme(i32),
    /// The monospaced family this many places along the installed ones.
    Font(i32),
    /// Open or close a list: every theme, or every font.
    List(List),
    /// The theme at this place in the library, picked from the list.
    PickTheme(usize),
    /// The font at this place in the font list.
    PickFont(usize),
    /// The theme list's last line: edit the theme in use, or a copy of a preset.
    Customize,
    Mode(Mode),
    OpenThemes,
    /// Open the keys file, for what Settings doesn't show (typed text).
    EditKeys,
    /// Wait for the next keys pressed, to bind this action to them.
    RecordKey(&'static str),
    /// This action back on its default keys.
    ResetKey(&'static str),
    /// Every action back on its default keys; typed text stays.
    ResetKeys,
    /// The binding that types text on this key, removed.
    RemoveTyped(Combo),
    Ligatures(bool),
    QuickTerminal(bool),
    Background(usize),
    Opacity(usize),
    Blur(bool),
    Shell(usize),
    /// What new tabs open with: 0 nothing, else 1 + an index into FETCHES.
    Fetch(usize),
    /// Open the picked fetch's config file.
    FetchConfig,
    TabsTop(bool),
    Extended(bool),
    Notifications(bool),
    ResumePrograms(bool),
    ResumeServers(bool),
    /// Smaller (-1), the default (0) or bigger (+1).
    Zoom(i32),
    Updates(bool),
    Analytics(bool),
    CheckForUpdates,
    /// End the session of the connected phone at this place in the list.
    Disconnect(usize),
    /// Sign in to remote control (true) or out.
    Remote(bool),
    /// Start typing in a text field.
    Edit(Field),
    /// The model from the provider at this place in llm::PROVIDERS.
    Provider(usize),
    /// Open the theme editor (true) or close it.
    ThemeEditor(bool),
    /// A theme of your own, copied from the one in use, to edit.
    DuplicateTheme,
    /// Delete the theme of your own in use: asks again first.
    DeleteTheme,
    /// The color the editor's hex field and nudges change.
    Slot(Slot),
    /// Turn the picked color this many notches along a channel.
    Nudge(Channel, i32),
    /// An index into cursor::Shape::ALL.
    Cursor(usize),
    Blink(bool),
    /// An index into LINE_SPACINGS.
    LineSpacing(usize),
    /// Show this sub tab of the page.
    Section(Section),
}

#[derive(Default)]
pub struct Page {
    /// Per frame: where each control is, in window coordinates.
    hits: Vec<(Rect, Action)>,
    scroll: f32,
    /// How tall the page was last drawn, and the tab it was drawn in.
    content_height: f32,
    view_height: f32,
    /// The list open (a click on the theme's or the font's name), where
    /// it was last drawn, and how far it's scrolled.
    pub open: Option<List>,
    list: Rect,
    list_scroll: f32,
    /// The sub tab showing.
    pub section: Section,
    /// Asked to show Remote Control (from the account menu).
    pub to_remote: bool,
    pub redraw: bool,
}

impl Page {
    /// The last drawn wins, so the theme list takes clicks over the rows under it.
    pub fn action_at(&self, x: f32, y: f32) -> Option<Action> {
        self.hits.iter().rev().find(|(rect, _)| rect.contains(x, y)).map(|&(_, action)| action)
    }

    /// Scrolls the open list when the mouse is over it, the page otherwise.
    pub fn wheel(&mut self, dy: f32, x: f32, y: f32) {
        if self.open.is_some() && self.list.contains(x, y) {
            self.list_scroll += dy * LIST_ROW;
            return;
        }
        let most = (self.content_height - self.view_height).max(0.0);
        self.scroll = (self.scroll + dy * WHEEL_STEP).clamp(0.0, most);
    }

    /// Switches to a sub tab, from its top, with no list open.
    pub fn show(&mut self, section: Section) {
        if self.section != section {
            self.section = section;
            self.scroll = 0.0;
            self.open = None;
        }
    }

    /// Opens a list scrolled to what's in use, or closes it.
    pub fn toggle(&mut self, list: List) {
        self.open = if self.open == Some(list) { None } else { Some(list) };
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
        // The account menu's Remote Control: its own tab, from the top.
        if std::mem::take(&mut self.to_remote) && values.remote.is_some() {
            self.show(Section::Remote);
        }
        // Signed-out builds without a relay have no Remote tab to stay on.
        if !Section::shown(values).contains(&self.section) {
            self.section = Section::default();
        }
        // The stored scroll was bounded by an older frame: content that
        // shrank (a section closed) or a window that grew can leave it
        // past the end, and the page would draw shifted and half-blank
        // until the next wheel. Clamp it before anything is placed.
        let most = (self.content_height - rect.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, most);
        let width = (rect.w - 2.0 * PAD).min(COLUMN);
        let x = rect.x + ((rect.w - width) / 2.0).max(PAD);
        let top = rect.y + PAD - self.scroll;
        scene.push_clip(rect);
        let mut ui = Ui { scene, text, theme, hits: &mut self.hits, mouse, clip: rect };

        let mut y = top;
        ui.text.draw_text(ui.scene, "Settings", x, y + 24.0, UI_BOLD, 22.0, theme.text);
        y += 52.0;

        let section = self.section;
        y = ui.section_bar(x, y, width, section, values);
        y = ui.sections(x, y, width, section, values);
        let version = format!("Temino {}", env!("CARGO_PKG_VERSION"));
        ui.text.draw_text(ui.scene, &version, x, y + 4.0, UI, 11.5, theme.text_faint);
        y += 24.0;

        let open = self.open;
        let anchor = ui.hits.iter().find(|&&(_, action)| Some(action) == open.map(Action::List)).map(|&(r, _)| r);
        match (anchor, open) {
            (Some(anchor), Some(list)) => {
                (self.list, self.list_scroll) = ui.list(list, anchor, values, self.list_scroll);
            }
            _ => self.open = None,
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
    /// Light or Dark.
    Mode,
    ThemeFolder,
    /// What the key rows do, and resetting them all.
    KeysHelp,
    /// An action and the keys that run it.
    Key(&'static str),
    /// A key that types text, at this place in the typed ones.
    Typed(usize),
    Ligatures,
    /// A few lines of terminal, in its font, spacing, colors and cursor.
    Sample,
    Font,
    QuickTerminal,
    Background,
    Opacity,
    Blur,
    Zoom,
    Shell,
    Fetch,
    FetchConfig,
    TabsTop,
    Extended,
    Notifications,
    ResumePrograms,
    ResumeServers,
    Updates,
    Analytics,
    CheckForUpdates,
    Remote,
    /// A phone in control now, at this place in the list.
    Connected(usize),
    /// What to do on the phone, with a QR code of its web app.
    Phone,
    Provider,
    Field(Field),
    /// The theme editor's rows.
    ThemeName,
    Colors(Group),
    SlotHex,
    Nudge,
    /// Done, Duplicate and Delete.
    EditorActions,
    LineSpacing,
    CursorShape,
    Blink,
}

/// A row of color dots in the theme editor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Base,
    Normal,
    Bright,
}

impl Row {
    const EDITOR: [Row; 7] = [
        Row::ThemeName,
        Row::Colors(Group::Base),
        Row::Colors(Group::Normal),
        Row::Colors(Group::Bright),
        Row::SlotHex,
        Row::Nudge,
        Row::EditorActions,
    ];

    fn height(self) -> f32 {
        match self {
            Row::Phone => PHONE_ROW,
            Row::Colors(_) => COLORS_ROW,
            Row::Key(_) => KEY_ROW,
            _ => ROW,
        }
    }
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
        let card = Rect::new(x, y + 22.0, width, rows.iter().map(|&row| self.row_height(row)).sum());
        self.scene.quad(Quad::new(card, theme.panel(0.04)).radius(10.0).border(1.0, theme.divider));
        let mut row_y = card.y;
        for (i, &row) in rows.iter().enumerate() {
            let row_rect = Rect::new(card.x, row_y, card.w, self.row_height(row));
            if i > 0 {
                self.scene.fill(Rect::new(row_rect.x + 16.0, row_rect.y, row_rect.w - 32.0, 1.0), theme.divider);
            }
            self.row(row_rect, row, values);
            row_y += row_rect.h;
        }
        card.bottom() + 28.0
    }

    /// The sample grows with the terminal's font and line spacing.
    fn row_height(&self, row: Row) -> f32 {
        match row {
            Row::Sample => preview::sample_height(self.text),
            _ => row.height(),
        }
    }

    fn row(&mut self, rect: Rect, row: Row, values: &Values) {
        let theme = self.theme;
        if let Row::Phone = row {
            return self.phone(rect, values);
        }
        if let Row::Colors(group) = row {
            return self.colors(rect, group, values);
        }
        if let Row::Sample = row {
            return self.sample(rect);
        }
        let (label, detail) = match row {
            Row::Theme => {
                ("Theme", format!("{} of {} · colors for the terminal and the window", values.theme, values.themes))
            }
            Row::Mode => ("Light or dark", "Every theme comes in both; the backgrounds follow".to_owned()),
            Row::KeysHelp | Row::Key(_) | Row::Typed(_) => self.key_label(row, values),
            Row::Font => ("Font", "The terminal's; any monospaced family installed".to_owned()),
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
                let names = backgrounds(values.mode);
                ("Background", values.background.map_or("The theme's own".to_owned(), |i| names[i].0.to_owned()))
            }
            Row::Opacity => ("Background opacity", "How much of the desktop shows through".to_owned()),
            Row::Blur => ("Blur behind the window", "Soften what shows through".to_owned()),
            Row::Zoom => (
                "Size",
                format!(
                    "{:.1} pt · {:.0}% · {} and {} too",
                    values.points,
                    values.zoom,
                    crate::platform::shortcut("+"),
                    crate::platform::shortcut("−")
                ),
            ),
            Row::Shell => ("Default shell", "For new tabs and panes; a missing one is installed".to_owned()),
            Row::Fetch => ("Fetch", "System info at the top of each new tab".to_owned()),
            Row::FetchConfig => {
                ("Fetch config", format!("What {} shows, and how", FETCHES[values.fetch.saturating_sub(1)].name))
            }
            Row::TabsTop => ("Tab list", "Down the side, or across the top".to_owned()),
            Row::Extended => ("Extended tab list", {
                // On Linux the command modifier is Ctrl+Shift already.
                let command = crate::platform::COMMAND_NAME;
                let keys = if cfg!(target_os = "macos") {
                    "⇧⌘B".to_owned()
                } else if command.contains("Shift") {
                    format!("{command}+B")
                } else {
                    format!("{command}+Shift+B")
                };
                format!("Under the active tab, what each pane is doing · {keys}")
            }),
            Row::Notifications => (
                "Desktop notifications",
                "When an agent waits or finishes, or a program asks, in a tab you can't see".to_owned(),
            ),
            Row::ResumePrograms => (
                "Reopen running programs",
                "At launch, coding agents pick up their conversation; monitors, tmux, logs, ssh and editors start again"
                    .to_owned(),
            ),
            Row::ResumeServers => (
                "Restart dev servers",
                "At launch, npm run dev, docker compose up and the like start again".to_owned(),
            ),
            Row::Updates => ("Check for updates daily", "At launch, from GitHub releases".to_owned()),
            Row::Analytics => (
                "Share anonymous usage data",
                "Helps us improve Temino — never private information, or anything you type in the terminal".to_owned(),
            ),
            Row::CheckForUpdates => ("Check now", "Offers a newer version if there is one".to_owned()),
            Row::Remote => (
                "Control from iPhone or iPad",
                if values.remote == Some(true) && !values.remote_account.is_empty() {
                    format!("Signed in as {} · end-to-end encrypted", values.remote_account)
                } else if values.remote == Some(true) {
                    "Signed in · end-to-end encrypted; new devices need your OK here".to_owned()
                } else {
                    "Sign in with Apple or Google, then on your phone too".to_owned()
                },
            ),
            Row::Phone | Row::Sample => return,
            Row::Connected(index) => (
                values.remote_phones[index].as_str(),
                "Connected · Disconnect takes back its OK: it has to ask again".to_owned(),
            ),
            Row::Provider => ("Model", "For AI mode and tab recaps".to_owned()),
            Row::Field(Field::LlmUrl) => ("Server", "OpenAI-compatible; list several with commas".to_owned()),
            Row::Field(Field::LlmKey) => {
                let provider = PROVIDERS[values.provider];
                let detail =
                    if provider.is("custom") { "If the server asks for one" } else { "Kept for this provider only" };
                ("API key", detail.to_owned())
            }
            Row::Field(Field::LlmModel) => ("Model name", "As the provider names it".to_owned()),
            Row::Field(Field::TypesafeKey) => ("TypeSafe key", "Lets Jev decide: command or answer".to_owned()),
            Row::Field(Field::ThemeName | Field::ThemeHex) | Row::Colors(_) => return,
            Row::ThemeName | Row::SlotHex | Row::Nudge | Row::EditorActions => self.editor_label(row, values),
            Row::LineSpacing => ("Line spacing", "Room between the terminal's lines".to_owned()),
            Row::CursorShape => (
                "Shape",
                match Shape::ALL[values.cursor] {
                    Shape::Auto => "A bar at the prompt, a block in programs".to_owned(),
                    shape => format!("{} at the prompt; programs that pick their own still do", shape.name()),
                },
            ),
            Row::Blink => ("Blink", "Steady when off".to_owned()),
        };
        let left = rect.x + 16.0;
        let right = rect.right() - 16.0;
        let control_y = rect.y + (rect.h - CONTROL_HEIGHT) / 2.0;
        // The control first, so the words know how much room they have:
        // in a narrow window, or a wider font (Linux's), a long description
        // is cut short with "…" rather than running under its button.
        let first_hit = self.hits.len();
        self.row_control(rect, row, values, right, control_y);
        let control_left = self.hits[first_hit..].iter().map(|(hit, _)| hit.x).fold(right, f32::min);
        let room = (control_left - 12.0 - left).max(0.0);
        let label = self.text.elide(label, UI, 13.0, room);
        let detail = self.text.elide(&detail, UI, 11.5, room);
        if detail.is_empty() {
            // One line, in the middle of the row.
            let baseline = (rect.y + (rect.h + self.text.ui_ascent(13.0) * 0.72) / 2.0).round();
            self.text.draw_text(self.scene, &label, left, baseline, UI, 13.0, theme.text);
            return;
        }
        self.text.draw_text(self.scene, &label, left, rect.y + 25.0, UI, 13.0, theme.text);
        self.text.draw_text(self.scene, &detail, left, rect.y + 42.0, UI, 11.5, theme.text_dim);
    }

    /// A row's control, right-aligned at `right`.
    fn row_control(&mut self, rect: Rect, row: Row, values: &Values, right: f32, control_y: f32) {
        match row {
            Row::Theme => {
                let labels = ["‹".to_owned(), values.theme.clone(), "›".to_owned()];
                self.segments(right, control_y, &labels, None, |i| match i {
                    1 => Action::List(List::Themes),
                    _ => Action::Theme(i as i32 - 1),
                });
            }
            Row::Mode => {
                let labels = Mode::ALL.map(|mode| mode.name().to_owned());
                let at = Mode::ALL.iter().position(|&m| m == values.mode);
                self.segments(right, control_y, &labels, at, |i| Action::Mode(Mode::ALL[i]));
            }
            Row::KeysHelp | Row::Key(_) | Row::Typed(_) => self.key_control(right, control_y, row, values),
            Row::Ligatures => self.toggle(right, rect.y, values.ligatures, Action::Ligatures(!values.ligatures)),
            Row::Font => {
                let labels = ["‹".to_owned(), values.font.clone(), "›".to_owned()];
                self.segments(right, control_y, &labels, None, |i| match i {
                    0 => Action::Font(-1),
                    1 => Action::List(List::Fonts),
                    _ => Action::Font(1),
                });
            }
            Row::QuickTerminal => {
                let on = values.quick_terminal.as_ref().is_some_and(Option::is_some);
                self.toggle(right, rect.y, on, Action::QuickTerminal(!on));
            }
            Row::ThemeFolder => self.button(right, control_y, "Open Themes Folder", Action::OpenThemes),
            Row::Background => self.swatches(right, rect.y + (ROW - SWATCH) / 2.0, values),
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
            Row::Fetch => {
                let mut labels = vec!["Off".to_owned()];
                labels.extend(FETCHES.iter().zip(values.fetches_installed).map(|(fetch, installed)| {
                    if installed {
                        fetch.name.to_owned()
                    } else {
                        format!("{} ↓", fetch.name)
                    }
                }));
                self.segments(right, control_y, &labels, Some(values.fetch), Action::Fetch);
            }
            Row::FetchConfig => self.button(right, control_y, "Edit Config", Action::FetchConfig),
            Row::TabsTop => {
                let labels = ["Side".to_owned(), "Top".to_owned()];
                self.segments(right, control_y, &labels, Some(values.tabs_top as usize), |i| Action::TabsTop(i == 1));
            }
            Row::Extended => self.toggle(right, rect.y, values.extended, Action::Extended(!values.extended)),
            Row::Notifications => {
                self.toggle(right, rect.y, values.notifications, Action::Notifications(!values.notifications))
            }
            Row::ResumePrograms => {
                self.toggle(right, rect.y, values.resume_programs, Action::ResumePrograms(!values.resume_programs))
            }
            Row::ResumeServers => {
                self.toggle(right, rect.y, values.resume_servers, Action::ResumeServers(!values.resume_servers))
            }
            Row::Updates => self.toggle(right, rect.y, values.updates, Action::Updates(!values.updates)),
            Row::Analytics => self.toggle(right, rect.y, values.analytics, Action::Analytics(!values.analytics)),
            Row::CheckForUpdates => self.button(right, control_y, "Check for Updates", Action::CheckForUpdates),
            Row::Remote => match values.remote {
                Some(true) => self.button(right, control_y, "Sign Out", Action::Remote(false)),
                _ => self.button(right, control_y, "Sign In…", Action::Remote(true)),
            },
            Row::Provider => {
                let labels = PROVIDERS.map(|p| p.name.to_owned());
                self.segments(right, control_y, &labels, Some(values.provider), Action::Provider);
            }
            Row::Field(field) => self.field(right, control_y, FIELD_WIDTH, field, values),
            Row::Connected(index) => self.button(right, control_y, "Disconnect", Action::Disconnect(index)),
            Row::Phone | Row::Sample | Row::Colors(_) => {}
            Row::ThemeName | Row::SlotHex | Row::Nudge | Row::EditorActions => {
                self.editor_control(right, control_y, row, values)
            }
            Row::LineSpacing => {
                let labels = LINE_SPACINGS.map(|(name, _)| name.to_owned());
                self.segments(right, control_y, &labels, Some(values.line_spacing), Action::LineSpacing);
            }
            Row::CursorShape => {
                let labels = Shape::ALL.map(|shape| shape.name().to_owned());
                self.segments(right, control_y, &labels, Some(values.cursor), Action::Cursor);
            }
            Row::Blink => self.toggle(right, rect.y, values.blink, Action::Blink(!values.blink)),
        }
    }
}

mod widgets;

#[cfg(test)]
mod tests;

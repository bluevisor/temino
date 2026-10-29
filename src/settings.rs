//! The Settings tab (Command+,): every setting on one page, drawn like the
//! rest of the window. The page only draws and says what a click asks for;
//! app.rs applies it and remembers it in the settings file.

use crate::scene::{Quad, Rect, Scene};
use crate::shells::KINDS;
use crate::text::{TextSystem, UI, UI_BOLD};
use crate::theme::{Color, Theme, BACKGROUNDS, OPACITIES};

/// The page's column, at most this wide, centered in the tab.
const COLUMN: f32 = 640.0;
const PAD: f32 = 32.0;
const ROW: f32 = 58.0;
const CONTROL_HEIGHT: f32 = 26.0;
const SWATCH: f32 = 22.0;
/// How far one wheel line scrolls the page.
const WHEEL_STEP: f32 = 48.0;

/// What the settings are now, for drawing them.
pub struct Values {
    pub background: usize,
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
}

/// What a click on the page asks for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Background(usize),
    Opacity(usize),
    Blur(bool),
    Shell(usize),
    TabsTop(bool),
    /// Smaller (-1), the default (0) or bigger (+1).
    Zoom(i32),
    Updates(bool),
    CheckForUpdates,
}

#[derive(Default)]
pub struct Page {
    /// Per frame: where each control is, in window coordinates.
    hits: Vec<(Rect, Action)>,
    scroll: f32,
    /// How tall the page was last drawn, and the tab it was drawn in.
    content_height: f32,
    view_height: f32,
}

impl Page {
    pub fn action_at(&self, x: f32, y: f32) -> Option<Action> {
        self.hits.iter().find(|(rect, _)| rect.contains(x, y)).map(|&(_, action)| action)
    }

    pub fn wheel(&mut self, dy: f32) {
        let most = (self.content_height - self.view_height).max(0.0);
        self.scroll = (self.scroll + dy * WHEEL_STEP).clamp(0.0, most);
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

        let mut rows = vec![Row::Background];
        if values.see_through {
            rows.extend([Row::Opacity, Row::Blur]);
        }
        y = ui.section(x, y, width, "Appearance", &rows, values);
        y = ui.section(x, y, width, "Text", &[Row::Zoom], values);
        y = ui.section(x, y, width, "Shell", &[Row::Shell], values);
        y = ui.section(x, y, width, "Tabs", &[Row::TabsTop], values);
        y = ui.section(x, y, width, "Updates", &[Row::Updates, Row::CheckForUpdates], values);
        let version = format!("Temino {}", env!("CARGO_PKG_VERSION"));
        ui.text.draw_text(ui.scene, &version, x, y + 4.0, UI, 11.5, theme.text_faint);
        y += 24.0;

        self.content_height = y + self.scroll - rect.y + PAD;
        scene.pop_clip();
    }
}

/// One row of the page: a label and a description on the left, its control
/// on the right.
#[derive(Clone, Copy)]
enum Row {
    Background,
    Opacity,
    Blur,
    Zoom,
    Shell,
    TabsTop,
    Updates,
    CheckForUpdates,
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
            Row::Background => ("Background", BACKGROUNDS[values.background].0.to_owned()),
            Row::Opacity => ("Background opacity", "How much of the desktop shows through".to_owned()),
            Row::Blur => ("Blur behind the window", "Soften what shows through".to_owned()),
            Row::Zoom => ("Size", format!("{:.0}% · Command+plus and Command+minus too", values.zoom)),
            Row::Shell => ("Default shell", "For new tabs and panes; a missing one is installed".to_owned()),
            Row::TabsTop => ("Tab list", "Down the side, or across the top".to_owned()),
            Row::Updates => ("Check for updates daily", "At launch, from GitHub releases".to_owned()),
            Row::CheckForUpdates => ("Check now", "Offers a newer version if there is one".to_owned()),
        };
        let left = rect.x + 16.0;
        self.text.draw_text(self.scene, label, left, rect.y + 25.0, UI, 13.0, theme.text);
        self.text.draw_text(self.scene, &detail, left, rect.y + 42.0, UI, 11.5, theme.text_dim);
        let right = rect.right() - 16.0;
        let control_y = rect.y + (ROW - CONTROL_HEIGHT) / 2.0;
        match row {
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
        }
    }

    /// The backgrounds as color dots, right-aligned at `right`.
    fn swatches(&mut self, right: f32, y: f32, selected: usize) {
        let theme = self.theme;
        let step = SWATCH + 10.0;
        let mut x = right - step * BACKGROUNDS.len() as f32 + 10.0;
        for (index, &(_, rgb)) in BACKGROUNDS.iter().enumerate() {
            let dot = Rect::new(x, y, SWATCH, SWATCH);
            let hit = dot.inset(-4.0, -4.0);
            let ring = if index == selected {
                theme.accent
            } else if self.hovered(hit) {
                theme.text_dim
            } else {
                Color::hex(0xffffff).alpha(0.18)
            };
            if index == selected {
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
            background: 0,
            opacity: 0,
            blur: false,
            shell: Some(0),
            installed: [true, true, false],
            tabs_top: false,
            zoom: 100.0,
            updates: true,
            see_through: true,
        };
        let mut page = Page::default();
        let rect = Rect::new(200.0, 0.0, 900.0, 2000.0);
        scene.reset(1100.0, 2000.0, 1.0, theme.background);
        page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
        let actions: Vec<Action> = page.hits.iter().map(|&(_, a)| a).collect();
        let mut expected: Vec<Action> = (0..BACKGROUNDS.len()).map(Action::Background).collect();
        expected.extend((0..OPACITIES.len()).map(Action::Opacity));
        expected.extend([Action::Blur(true), Action::Zoom(-1), Action::Zoom(0), Action::Zoom(1)]);
        expected.extend((0..KINDS.len()).map(Action::Shell));
        expected.extend([
            Action::TabsTop(false),
            Action::TabsTop(true),
            Action::Updates(false),
            Action::CheckForUpdates,
        ]);
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
        page.wheel(100.0);
        assert_eq!(page.scroll, 300.0);
        page.wheel(-100.0);
        assert_eq!(page.scroll, 0.0);
    }
}

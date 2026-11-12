//! The lists that drop from a name in Settings: every theme (each with a
//! chip of its colors, and Customize… at the foot), and every font.

use super::*;

/// Which list is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum List {
    Themes,
    Fonts,
}

/// One line of a list: its name, and for a theme its background and four
/// of its colors.
struct Item<'a> {
    name: &'a str,
    chip: Option<(u32, [u32; 4])>,
}

impl Ui<'_> {
    /// `list` dropped under `anchor` (the name clicked), the one in use
    /// lit. `scroll` NaN starts it on the one in use; returns where the
    /// list is and the scroll kept in range.
    pub(super) fn list(&mut self, list: List, anchor: Rect, values: &Values, scroll: f32) -> (Rect, f32) {
        match list {
            List::Themes => {
                // Colors from the theme each makes: Temino's own are worked
                // out in code, not listed in its palette.
                let rgb = |color: Color| {
                    let [r, g, b] = color.to_rgb8();
                    (r as u32) << 16 | (g as u32) << 8 | b as u32
                };
                let items: Vec<Item> = values
                    .palettes
                    .iter()
                    .map(|p| {
                        let shown = p.theme();
                        let chip = (rgb(shown.background), [1, 2, 4, 5].map(|i| rgb(shown.ansi[i])));
                        Item { name: &p.name, chip: Some(chip) }
                    })
                    .collect();
                let foot = ("Customize…", Action::Customize);
                self.dropdown(anchor, &items, values.palette, scroll, Action::PickTheme, Some(foot))
            }
            List::Fonts => {
                let items: Vec<Item> = values.fonts.iter().map(|name| Item { name, chip: None }).collect();
                self.dropdown(anchor, &items, values.font_index, scroll, Action::PickFont, None)
            }
        }
    }

    fn dropdown(
        &mut self,
        anchor: Rect,
        items: &[Item],
        selected: usize,
        scroll: f32,
        pick: fn(usize) -> Action,
        foot: Option<(&str, Action)>,
    ) -> (Rect, f32) {
        let theme = self.theme;
        let size = 12.5;
        let chip_w = if items.iter().any(|item| item.chip.is_some()) { 56.0 } else { 0.0 };
        let widest = items.iter().map(|item| self.text.measure(item.name, UI, size)).fold(0.0, f32::max);
        let w = (widest + chip_w + 40.0).max(220.0);
        let content = LIST_ROW * items.len() as f32;
        let foot_h = if foot.is_some() { LIST_ROW + 9.0 } else { 0.0 };
        let h = content.min(LIST_HEIGHT) + 8.0 + foot_h;
        let below = self.clip.bottom() - anchor.bottom() - 12.0;
        let y = if below >= h || below >= anchor.y - self.clip.y { anchor.bottom() + 6.0 } else { anchor.y - 6.0 - h };
        let x = (anchor.x + anchor.w / 2.0 - w / 2.0).min(self.clip.right() - w - 12.0).max(self.clip.x + 12.0);
        let rect = Rect::new(x, y, w, h);
        let view = h - 8.0 - foot_h;
        let most = (content - view).max(0.0);
        let scroll = if scroll.is_nan() { LIST_ROW * selected as f32 - (view - LIST_ROW) / 2.0 } else { scroll };
        let scroll = scroll.clamp(0.0, most);

        self.scene.set_layer(LAYER_OVERLAY);
        let shadow = Rect::new(rect.x - 1.0, rect.y + 5.0, rect.w + 2.0, rect.h + 2.0);
        self.scene.quad(Quad::new(shadow, theme.shadow(0.28)).radius(12.0));
        self.scene.quad(Quad::new(rect, theme.raised).radius(10.0).border(1.0, theme.divider));
        // The list's padding takes clicks too, rather than the rows under it.
        self.hit(rect, pick(selected));
        let inner = Rect::new(rect.x + 4.0, rect.y + 4.0, rect.w - 8.0, view);
        self.scene.push_clip(inner);
        let ascent = self.text.ui_ascent(size);
        for (index, item) in items.iter().enumerate() {
            let line = Rect::new(inner.x, inner.y + index as f32 * LIST_ROW - scroll, inner.w, LIST_ROW);
            if line.bottom() < inner.y || line.y > inner.bottom() {
                continue;
            }
            let on = index == selected;
            self.line_fill(line, on, inner);
            let mut text_x = line.x + 12.0;
            if let Some((background, dots)) = item.chip {
                let chip = Rect::new(line.x + 8.0, line.y + 6.0, 44.0, LIST_ROW - 12.0);
                let chip_quad = Quad::new(chip, Color::hex(background)).radius(4.0);
                self.scene.quad(chip_quad.border(1.0, theme.lift().alpha(0.18)));
                for (i, &rgb) in dots.iter().enumerate() {
                    let dot = Rect::new(chip.x + 5.0 + i as f32 * 9.0, chip.y + chip.h / 2.0 - 3.0, 6.0, 6.0);
                    self.scene.quad(Quad::new(dot, Color::hex(rgb)).radius(3.0));
                }
                text_x = chip.right() + 12.0;
            }
            let baseline = (line.y + (line.h + ascent * 0.72) / 2.0).round();
            let color = if on { self.on_accent() } else { theme.text };
            self.text.draw_text(self.scene, item.name, text_x, baseline, UI, size, color);
            let (top, bottom) = (line.y.max(inner.y), line.bottom().min(inner.bottom()));
            self.hit(Rect::new(line.x, top, line.w, bottom - top), pick(index));
        }
        self.scene.pop_clip();
        if let Some((label, action)) = foot {
            // Pinned under the list, however far it's scrolled.
            let rule = inner.bottom() + 4.0;
            self.scene.fill(Rect::new(rect.x + 10.0, rule, rect.w - 20.0, 1.0), theme.divider);
            let line = Rect::new(inner.x, rule + 5.0, inner.w, LIST_ROW);
            self.line_fill(line, false, line);
            let baseline = (line.y + (line.h + ascent * 0.72) / 2.0).round();
            self.text.draw_text(self.scene, label, line.x + 12.0, baseline, UI, size, theme.accent);
            self.hit(line, action);
        }
        self.scene.set_layer(LAYER_CONTENT);
        (rect, scroll)
    }

    /// A list line's fill: the accent when it's the one in use, a faint
    /// one under the pointer (within `area`).
    fn line_fill(&mut self, line: Rect, on: bool, area: Rect) {
        let theme = self.theme;
        if on {
            self.scene.quad(Quad::new(line, theme.accent.alpha(0.85)).radius(6.0));
        } else if self.hovered(line) && area.contains(self.mouse.0, self.mouse.1) {
            self.scene.quad(Quad::new(line, theme.lift().alpha(0.07)).radius(6.0));
        }
    }
}

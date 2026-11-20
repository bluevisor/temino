//! Zen mode: the current tab alone, on a card with a hairline edge and a
//! soft shadow, a small margin of window around it. No tab list, title
//! bar, pane headers or traffic lights; the same shortcut brings them back.

use std::cell::Cell;

use super::*;

/// The margin around the tab, in points: also how tall the title bar
/// stays on macOS, to drag the window by.
const ZEN_MARGIN: f32 = 10.0;
/// The card's corners.
const ZEN_RADIUS: f32 = 8.0;

thread_local! {
    /// What this window (its core thread) last told the platform.
    static SENT: Cell<Option<bool>> = const { Cell::new(None) };
}

impl App {
    /// ⇧⌘↩: into zen mode or out of it, remembered for next launch.
    pub(super) fn toggle_zen(&mut self) {
        self.zen = !self.zen;
        crate::save_setting("zen", if self.zen { "on" } else { "off" });
        self.relayout();
        if self.zen {
            let key =
                if cfg!(target_os = "macos") { "⇧⌘↩".to_owned() } else { platform::shortcut("Shift+Enter") };
            self.ctx.show_toast(&format!("Zen mode · {key} leaves it"));
        }
    }
    /// The tab's place in zen mode: the window less its margin.
    pub(super) fn zen_frame(&self) -> Rect {
        let margin = ZEN_MARGIN / self.zoom;
        Rect::new(margin, margin, (self.width - 2.0 * margin).max(1.0), (self.height - 2.0 * margin).max(1.0))
    }
    /// Tells the window (once per change) to hide or bring back its title
    /// bar's controls.
    pub(super) fn sync_zen(&self) {
        if SENT.get() != Some(self.zen) {
            SENT.set(Some(self.zen));
            platform::set_zen(self.zen, ZEN_MARGIN);
        }
    }
    /// Under the tab: the margin a shade darker, and the card's shadow
    /// falling on it (only on it: the card itself stays the window's color).
    pub(super) fn draw_zen_ground(&mut self) {
        let card = self.zen_frame();
        let (w, h) = (self.width, self.height);
        let theme = &self.ctx.theme;
        let (shade, shadow) = (theme.shadow(0.12), theme.shadow(0.32));
        let scene = &mut self.scene;
        let strips = [
            Rect::new(0.0, 0.0, w, card.y),
            Rect::new(0.0, card.bottom(), w, h - card.bottom()),
            Rect::new(0.0, card.y, card.x, card.h),
            Rect::new(card.right(), card.y, w - card.right(), card.h),
        ];
        let corners = [
            Rect::new(card.x, card.y, ZEN_RADIUS, ZEN_RADIUS),
            Rect::new(card.right() - ZEN_RADIUS, card.y, ZEN_RADIUS, ZEN_RADIUS),
            Rect::new(card.x, card.bottom() - ZEN_RADIUS, ZEN_RADIUS, ZEN_RADIUS),
            Rect::new(card.right() - ZEN_RADIUS, card.bottom() - ZEN_RADIUS, ZEN_RADIUS, ZEN_RADIUS),
        ];
        let dropped = Rect::new(card.x, card.y + 2.0, card.w, card.h);
        for strip in strips {
            scene.push_clip(strip);
            scene.fill(strip, shade);
            scene.shadow(dropped, shadow, ZEN_RADIUS, 6.0);
            scene.pop_clip();
        }
        // The rounded corners show the margin's shade, cut to the curve
        // by the card's own fill drawn back over them.
        for corner in corners {
            scene.push_clip(corner);
            scene.fill(corner, shade);
            scene.shadow(dropped, shadow, ZEN_RADIUS, 6.0);
            scene.quad(Quad::new(card, theme.window()).radius(ZEN_RADIUS));
            scene.pop_clip();
        }
    }
    /// Over the tab: its hairline edge.
    pub(super) fn draw_zen_edge(&mut self) {
        let card = self.zen_frame();
        let edge = self.ctx.theme.divider;
        self.scene.set_layer(LAYER_OVERLAY);
        self.scene.quad(Quad::new(card, Color::TRANSPARENT).radius(ZEN_RADIUS).border(1.0, edge));
        self.scene.set_layer(LAYER_CONTENT);
    }
}

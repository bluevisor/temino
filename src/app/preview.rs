use super::*;
use crate::settings::Preview;

impl App {
    /// Whatever choice the pointer is over in Settings (a theme or font in
    /// its list, a background, a line spacing, a cursor shape, an opacity)
    /// shows everywhere at once, as picking it would; off it, or with
    /// Settings out of sight, the one in use is back. Only a change of
    /// what's under the pointer does any work.
    pub(super) fn preview(&mut self) {
        let (x, y) = self.mouse;
        let hovered = match self.pane_at(x, y) {
            Some((SETTINGS, _)) => self.settings.action_at(x, y).and_then(|a| Preview::of(a, self.settings.open)),
            _ => None,
        };
        if hovered == self.previewing {
            return;
        }
        if let Some(old) = self.previewing.take() {
            // Another value of the same setting replaces it outright.
            if !hovered.is_some_and(|new| new.same_setting(old)) {
                self.end_preview(old);
            }
        }
        self.previewing = hovered;
        if let Some(new) = hovered {
            self.show_preview(new);
        }
        self.dirty = true;
    }

    fn show_preview(&mut self, preview: Preview) {
        self.preview_failed = false;
        match preview {
            Preview::Theme(index) => {
                let Some(palette) = self.themes.get(index) else { return };
                // Picked, a theme drops the background preset.
                let mode = self.mode.unwrap_or_else(|| palette.mode());
                self.ctx.theme = build_theme(palette, None, self.ctx.theme.opacity, mode);
                platform::set_background(self.ctx.theme.background.to_rgb8());
            }
            Preview::Background(index) => {
                let mode = self.shown_mode();
                if index >= backgrounds(mode).len() {
                    return;
                }
                let palette = self.themes.get(self.palette).unwrap_or(&self.themes[0]);
                self.ctx.theme = build_theme(palette, Some(index), self.ctx.theme.opacity, mode);
                platform::set_background(self.ctx.theme.background.to_rgb8());
            }
            Preview::Opacity(index) => {
                let Some(&percent) = OPACITIES.get(index) else { return };
                self.ctx.theme.opacity = percent as f32 / 100.0;
                self.apply_blur();
            }
            Preview::Font(index) => {
                let Some(choice) = self.font_choices.get(index).cloned() else { return };
                // One that won't load just doesn't show; picking it says so.
                match self.ctx.text.set_mono(choice.as_deref()) {
                    Ok(()) => self.reflow(),
                    Err(error) => {
                        crate::trace(&format!("font preview {choice:?}: {error}"));
                        self.preview_failed = true;
                    }
                }
            }
            Preview::LineSpacing(index) => {
                let Some(&(_, leading)) = crate::text::LINE_SPACINGS.get(index) else { return };
                self.set_leading(leading);
            }
            Preview::Cursor(index) => {
                crate::cursor::preview(crate::cursor::Shape::ALL.get(index).copied());
                self.ctx.blink_epoch = Instant::now();
            }
        }
    }

    /// Back to what's in use (and saved) for the setting `preview` showed.
    fn end_preview(&mut self, preview: Preview) {
        match preview {
            Preview::Theme(_) | Preview::Background(_) => self.apply_theme(),
            Preview::Opacity(_) => {
                self.ctx.theme.opacity = OPACITIES[self.opacity.min(OPACITIES.len() - 1)] as f32 / 100.0;
                self.apply_blur();
            }
            Preview::Font(_) => {
                if std::mem::take(&mut self.preview_failed) {
                    return; // the font in use never left
                }
                let saved = crate::setting("font").filter(|f| !f.is_empty());
                match self.ctx.text.set_mono(saved.as_deref()) {
                    Ok(()) => self.reflow(),
                    Err(error) => crate::trace(&format!("font {saved:?}: {error}")),
                }
            }
            Preview::LineSpacing(_) => self.set_leading(load_line_spacing()),
            Preview::Cursor(_) => crate::cursor::preview(None),
        }
    }

    /// Settings ▸ Font list: the font at `index` picked. Already showing
    /// as a preview, it's only saved, not loaded a second time.
    pub(super) fn commit_font(&mut self, index: usize) {
        let shown = self.previewing == Some(Preview::Font(index)) && !self.preview_failed;
        self.previewing = None;
        self.preview_failed = false;
        match self.font_choices.get(index) {
            Some(choice) if shown => crate::save_setting("font", choice.as_deref().unwrap_or("")),
            _ => self.pick_font(index),
        }
    }

    /// Lines this many times the font's height, if that's a change.
    pub(super) fn set_leading(&mut self, leading: f32) {
        if leading != self.ctx.text.leading() {
            self.ctx.text.set_leading(leading);
            self.reflow();
        }
    }

    /// Cells changed size: every pane lays out its text again.
    pub(super) fn reflow(&mut self) {
        for pane in &mut self.panes {
            pane.cells_changed(&self.ctx.text);
        }
        self.relayout();
    }
}

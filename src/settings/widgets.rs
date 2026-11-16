//! The Settings page's controls: buttons, switches, joined segments, text
//! fields, color dots and the phone's QR code.

use super::*;

impl Ui<'_> {
    /// Remote Control's other end, step by step, beside a QR code that
    /// opens the phone's web app.
    pub(super) fn phone(&mut self, rect: Rect, values: &Values) {
        let theme = self.theme;
        let left = rect.x + 16.0;
        self.text.draw_text(self.scene, "On your iPhone or iPad", left, rect.y + 25.0, UI, 13.0, theme.text);
        let address = values.remote_url.split('#').next().unwrap_or_default().trim_start_matches("https://");
        let open = format!("Scan the code, or open {address}");
        // The account's email once signed in (before that, or for a machine
        // signed in before the relay passed it on, it may be a first name).
        let same = if values.remote_account.contains('@') {
            format!("Sign in there as {}", values.remote_account)
        } else {
            "Sign in with the same Apple or Google account".to_owned()
        };
        let mut steps = Vec::new();
        if values.remote != Some(true) {
            steps.push("Sign in here first, with Sign In… above");
        }
        steps.extend([
            open.as_str(),
            same.as_str(),
            "Allow it here: Temino shows the code the phone shows",
            "For agent alerts: Share ▸ Add to Home Screen",
        ]);
        let mut y = rect.y + 48.0;
        for (number, step) in steps.iter().enumerate() {
            let number = format!("{}", number + 1);
            self.text.draw_text(self.scene, &number, left + 2.0, y, UI_BOLD, 11.5, theme.accent);
            self.text.draw_text(self.scene, step, left + 18.0, y, UI, 11.5, theme.text_dim);
            y += 22.0;
        }
        let code = Rect::new(rect.right() - 16.0 - QR_SIZE, rect.y + (rect.h - QR_SIZE) / 2.0, QR_SIZE, QR_SIZE);
        self.qr(code, &values.remote_url);
    }

    /// `text` as a QR code filling `rect`, dark on white with its quiet zone.
    pub(super) fn qr(&mut self, rect: Rect, text: &str) {
        let Ok(qr) = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) else { return };
        self.scene.quad(Quad::new(rect, Color::hex(0xffffff)).radius(8.0));
        let size = qr.size();
        // Two modules of quiet zone each side (the white card's edge adds more).
        let module = (rect.w / (size + 4) as f32).floor();
        let origin = rect.x + ((rect.w - module * size as f32) / 2.0).round();
        let top = rect.y + ((rect.h - module * size as f32) / 2.0).round();
        let ink = Color::hex(0x000000);
        for row in 0..size {
            // Each run of dark modules in a row as one rectangle.
            let mut column = 0;
            while column < size {
                if !qr.get_module(column, row) {
                    column += 1;
                    continue;
                }
                let start = column;
                while column < size && qr.get_module(column, row) {
                    column += 1;
                }
                let x = origin + start as f32 * module;
                let y = top + row as f32 * module;
                self.scene.fill(Rect::new(x, y, (column - start) as f32 * module, module), ink);
            }
        }
    }

    /// A text field, right-aligned at `right`: the setting's value (keys as
    /// dots), or what's being typed with a caret.
    pub(super) fn field(&mut self, right: f32, y: f32, width: f32, field: Field, values: &Values) {
        let theme = self.theme;
        let size = 12.0;
        let rect = Rect::new(right - width, y, width, CONTROL_HEIGHT);
        let editing = values.editing.as_ref().filter(|(f, _, _)| *f == field);
        let border = if editing.is_some() {
            theme.accent
        } else if self.hovered(rect) {
            theme.text_faint
        } else {
            theme.divider
        };
        self.scene.quad(Quad::new(rect, self.well()).radius(7.0).border(1.0, border));
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
            None if field.index().is_none() => {
                let text = editor::field_text(field, values);
                self.text.draw_text(self.scene, &text, x, baseline, UI, size, theme.text);
            }
            None => {
                let value = field.index().map_or("", |i| values.fields[i].as_str());
                let provider = PROVIDERS[values.provider];
                let (text, color) = match (value.is_empty(), field) {
                    (true, Field::LlmUrl) => ("http://host:8080/v1".to_owned(), theme.text_faint),
                    (true, Field::LlmModel) if provider.model.is_empty() => {
                        ("The first it lists".to_owned(), theme.text_faint)
                    }
                    (true, Field::LlmModel) => (provider.model.to_owned(), theme.text_faint),
                    (true, _) => ("Not set".to_owned(), theme.text_faint),
                    (false, _) => (shown(value), theme.text),
                };
                self.text.draw_text(self.scene, &text, x, baseline, UI, size, color);
            }
        }
        self.scene.pop_clip();
        self.hit(rect, Action::Edit(field));
    }

    /// The backgrounds as color dots, right-aligned at `right`: the light
    /// ones in Light mode.
    pub(super) fn swatches(&mut self, right: f32, y: f32, values: &Values) {
        let theme = self.theme;
        let selected = values.background;
        let list = backgrounds(values.mode);
        let step = SWATCH + 10.0;
        let mut x = right - step * list.len() as f32 + 10.0;
        for (index, &(_, rgb)) in list.iter().enumerate() {
            let dot = Rect::new(x, y, SWATCH, SWATCH);
            let hit = dot.inset(-4.0, -4.0);
            let ring = if Some(index) == selected {
                theme.accent
            } else if self.hovered(hit) {
                theme.text_dim
            } else {
                theme.lift().alpha(0.18)
            };
            if Some(index) == selected {
                self.scene.quad(Quad::new(dot.inset(-3.0, -3.0), Color::TRANSPARENT).radius(SWATCH).border(2.0, ring));
                self.scene
                    .quad(Quad::new(dot, Color::hex(rgb)).radius(SWATCH / 2.0).border(1.0, theme.lift().alpha(0.18)));
            } else {
                self.scene.quad(Quad::new(dot, Color::hex(rgb)).radius(SWATCH / 2.0).border(1.0, ring));
            }
            self.hit(hit, Action::Background(index));
            x += step;
        }
    }

    /// A row of joined buttons, right-aligned at `right`, `selected` lit.
    fn segment_widths(&mut self, labels: &[String]) -> Vec<f32> {
        labels.iter().map(|l| (self.text.measure(l, UI, 12.0) + 20.0).max(34.0)).collect()
    }

    /// How wide `segments` draws these labels, but for its frame.
    pub(super) fn segments_width(&mut self, labels: &[String]) -> f32 {
        self.segment_widths(labels).iter().sum()
    }

    pub(super) fn segments(
        &mut self,
        right: f32,
        y: f32,
        labels: &[String],
        selected: Option<usize>,
        action: impl Fn(usize) -> Action,
    ) -> f32 {
        let theme = self.theme;
        let size = 12.0;
        let widths = self.segment_widths(labels);
        let total: f32 = widths.iter().sum();
        let outer = Rect::new(right - total - 4.0, y, total + 4.0, CONTROL_HEIGHT);
        self.scene.quad(Quad::new(outer, self.well()).radius(7.0).border(1.0, theme.divider));
        let ascent = self.text.ui_ascent(size);
        let mut x = outer.x + 2.0;
        for (index, (label, w)) in labels.iter().zip(widths).enumerate() {
            let segment = Rect::new(x, y + 2.0, w, CONTROL_HEIGHT - 4.0);
            let on = selected == Some(index);
            if on {
                self.scene.quad(Quad::new(segment, theme.accent.alpha(0.85)).radius(5.0));
            } else if self.hovered(segment) {
                self.scene.quad(Quad::new(segment, theme.lift().alpha(0.07)).radius(5.0));
            }
            let label_width = self.text.measure(label, UI, size);
            let baseline = (segment.y + (segment.h + ascent * 0.72) / 2.0).round();
            let color = if on { self.on_accent() } else { theme.text };
            self.text.draw_text(self.scene, label, segment.x + (w - label_width) / 2.0, baseline, UI, size, color);
            self.hit(segment, action(index));
            x += w;
        }
        outer.x
    }

    /// An on/off switch, right-aligned at `right` and centered in the row at `row_y`.
    pub(super) fn toggle(&mut self, right: f32, row_y: f32, on: bool, action: Action) {
        let theme = self.theme;
        let (w, h) = (38.0, 22.0);
        let track = Rect::new(right - w, row_y + (ROW - h) / 2.0, w, h);
        let fill = if on { theme.accent } else { theme.lift().alpha(if self.hovered(track) { 0.18 } else { 0.12 }) };
        self.scene.quad(Quad::new(track, fill).radius(h / 2.0));
        let knob = h - 4.0;
        let kx = if on { track.right() - knob - 2.0 } else { track.x + 2.0 };
        self.scene.quad(Quad::new(Rect::new(kx, track.y + 2.0, knob, knob), Color::hex(0xf4f6fa)).radius(knob / 2.0));
        self.hit(track.inset(-6.0, -6.0), action);
    }

    /// The sunken fill under fields and segments.
    pub(super) fn well(&self) -> Color {
        Color::hex(0x000000).alpha(if self.theme.light() { 0.06 } else { 0.22 })
    }

    /// Words on the accent's fill: dark on a light accent, white on a dark one.
    pub(super) fn on_accent(&self) -> Color {
        let a = self.theme.accent;
        if 0.2126 * a.r + 0.7152 * a.g + 0.0722 * a.b > 0.45 {
            Color::hex(0x0b0d12)
        } else {
            Color::hex(0xffffff)
        }
    }

    pub(super) fn button(&mut self, right: f32, y: f32, label: &str, action: Action) {
        self.placed_button(right, y, label, action);
    }

    /// A button, right-aligned at `right`; returns its left edge, for the
    /// next one along.
    pub(super) fn placed_button(&mut self, right: f32, y: f32, label: &str, action: Action) -> f32 {
        let theme = self.theme;
        let size = 12.0;
        let w = self.text.measure(label, UI, size) + 24.0;
        let rect = Rect::new(right - w, y, w, CONTROL_HEIGHT);
        let fill = if self.hovered(rect) { theme.raised.mix(theme.lift(), 0.06) } else { theme.raised };
        self.scene.quad(Quad::new(rect, fill).radius(7.0).border(1.0, theme.divider));
        let baseline = (rect.y + (rect.h + self.text.ui_ascent(size) * 0.72) / 2.0).round();
        self.text.draw_text(self.scene, label, rect.x + 12.0, baseline, UI, size, theme.text);
        self.hit(rect, action);
        rect.x
    }
}

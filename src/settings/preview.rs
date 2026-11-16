//! Live previews: a choice under the pointer shows everywhere as if
//! picked, until the pointer moves off it; and the sample terminal at the
//! top of Text, where they show while Settings fills the window.

use super::*;

/// A choice in Settings the pointer is over, shown for now in place of
/// the one in use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preview {
    /// At this place in the theme list.
    Theme(usize),
    /// At this place in the font list.
    Font(usize),
    Background(usize),
    LineSpacing(usize),
    /// An index into cursor::Shape::ALL.
    Cursor(usize),
    Opacity(usize),
}

impl Preview {
    /// What hovering the control for `action` previews, with `open` the
    /// list that's open; None for a control that changes nothing visible.
    pub fn of(action: Action, open: Option<List>) -> Option<Preview> {
        Some(match action {
            Action::PickTheme(index) if open == Some(List::Themes) => Preview::Theme(index),
            Action::PickFont(index) if open == Some(List::Fonts) => Preview::Font(index),
            Action::Background(index) => Preview::Background(index),
            Action::LineSpacing(index) => Preview::LineSpacing(index),
            Action::Cursor(index) => Preview::Cursor(index),
            Action::Opacity(index) => Preview::Opacity(index),
            _ => return None,
        })
    }

    /// Another value of the same setting: moving between them needs no
    /// undoing in between.
    pub fn same_setting(self, other: Preview) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

/// The sample's lines: pieces of text, each in an ANSI color (None: the
/// theme's text color).
const SAMPLE: [&[(&str, Option<usize>)]; 4] = [
    &[("~/code/temino", Some(4)), (" main", Some(5)), (" ❯ ", Some(2)), ("cargo test", None)],
    &[("running 64 tests ", None), ("-> != >= {} [] 0O 1lI", Some(3))],
    &[("test result: ", None), ("ok", Some(2)), (". 64 passed; ", None), ("0 failed", Some(1))],
    &[("~/code/temino", Some(4)), (" main", Some(5)), (" ❯ ", Some(2))],
];
/// Space around the sample's text inside its box, and around the box.
const SAMPLE_PAD: f32 = 12.0;
const SAMPLE_MARGIN: f32 = 14.0;

/// The sample's line pitch, in ems before line spacing: roomy enough for
/// the tallest monospaced fonts (Monaco's lines are 1.34 em).
const SAMPLE_LINE: f32 = 1.35;

/// How tall the sample row is for the terminal's size and line spacing.
/// Not its font: fonts' lines run from 1.1 to 1.35 em, and picking one
/// mustn't resize the box and move the page under the pointer.
pub(super) fn sample_height(text: &TextSystem) -> f32 {
    let lines = SAMPLE.len() as f32 * (text.cell.size * SAMPLE_LINE * text.leading()).round();
    2.0 * (SAMPLE_PAD + SAMPLE_MARGIN) + lines
}

impl Ui<'_> {
    /// A few lines of a terminal in the terminal's own font, line spacing,
    /// colors and cursor, so a preview shows here too.
    pub(super) fn sample(&mut self, rect: Rect) {
        let theme = self.theme;
        let screen = Rect::new(
            rect.x + SAMPLE_MARGIN,
            rect.y + SAMPLE_MARGIN,
            rect.w - 2.0 * SAMPLE_MARGIN,
            rect.h - 2.0 * SAMPLE_MARGIN,
        );
        self.scene.quad(Quad::new(screen, theme.background).radius(8.0).border(1.0, theme.divider));
        self.scene.push_clip(screen);
        let cell = self.text.cell;
        let x = screen.x + SAMPLE_PAD;
        // The lines centered in the box, which doesn't fit itself to the font.
        let room = screen.h - 2.0 * SAMPLE_PAD - SAMPLE.len() as f32 * cell.height;
        let mut y = screen.y + SAMPLE_PAD + (room / 2.0).round();
        let mut end = x;
        for line in SAMPLE {
            let mut pen = x;
            for &(piece, ansi) in line {
                let color = ansi.map_or(theme.text, |i| theme.ansi[i]);
                pen += self.text.draw_mono(self.scene, piece, pen, y, Style::default(), color);
            }
            end = pen;
            y += cell.height;
        }
        // The cursor at the last prompt, steady, in the shape picked.
        let top = y - cell.height;
        let caret = match crate::cursor::prompt() {
            Shape::Block => Rect::new(end, top, cell.width, cell.height),
            Shape::Underline => Rect::new(end, top + cell.height - 2.0, cell.width, 2.0),
            _ => Rect::new(end, top + 1.0, 2.0, cell.height - 2.0),
        };
        self.scene.fill(caret, theme.cursor);
        self.scene.pop_clip();
    }
}

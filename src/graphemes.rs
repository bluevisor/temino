//! Emoji sequences as one character on the grid. alacritty_terminal gives
//! each code point its own width, but programs (Claude Code, anything using
//! string-width or wcswidth with grapheme support) count an emoji sequence
//! as two columns: ❤️ (a text-style character plus U+FE0F), 👍🏽 (a skin
//! tone) and 👨‍👩‍👧 (emoji joined with U+200D). Where the counts disagree,
//! every later cursor move lands in the wrong column. `Graphemes` sits
//! between the parser and the terminal and folds those sequences into the
//! first character's cell.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::cursor_icon::CursorIcon;
use alacritty_terminal::vte::ansi::*;

const VS16: char = '\u{fe0f}';
const ZWJ: char = '\u{200d}';

pub struct Graphemes<'a, T> {
    pub term: &'a mut Term<T>,
    /// The character `input` last saw, until anything else happens.
    pub last: &'a mut Option<char>,
}

impl<T: EventListener> Graphemes<'_, T> {
    /// The cell the last character went into (as alacritty puts zero-width
    /// characters).
    fn previous(&self) -> (Line, Column) {
        let cursor = &self.term.grid().cursor;
        let mut column = cursor.point.column;
        if !cursor.input_needs_wrap {
            column.0 = column.saturating_sub(1);
        }
        if self.term.grid()[cursor.point.line][column].flags.contains(Flags::WIDE_CHAR_SPACER) {
            column.0 = column.saturating_sub(1);
        }
        (cursor.point.line, column)
    }

    fn previous_is_wide(&self) -> bool {
        let (line, column) = self.previous();
        self.term.grid()[line][column].flags.contains(Flags::WIDE_CHAR)
    }

    /// Joins `c` to the previous cell without taking a column.
    fn attach(&mut self, c: char) {
        let (line, column) = self.previous();
        self.term.grid_mut()[line][column].push_zerowidth(c);
    }

    /// U+FE0F after a one-column character: it becomes a two-column emoji.
    fn widen(&mut self) -> bool {
        let columns = self.term.columns();
        let grid = self.term.grid_mut();
        let point = grid.cursor.point;
        // The character must be just left of the cursor, with room after it.
        if grid.cursor.input_needs_wrap || point.column.0 == 0 || point.column.0 >= columns {
            return false;
        }
        let base = Column(point.column.0 - 1);
        let template = grid.cursor.template.clone();
        let row = &mut grid[point.line];
        if row[base].flags.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER) {
            return false;
        }
        row[base].flags.insert(Flags::WIDE_CHAR);
        row[base].push_zerowidth(VS16);
        let spacer = &mut row[point.column];
        *spacer = template;
        spacer.c = ' ';
        spacer.flags.insert(Flags::WIDE_CHAR_SPACER);
        if point.column.0 + 1 < columns {
            grid.cursor.point.column.0 += 1;
        } else {
            grid.cursor.input_needs_wrap = true;
        }
        true
    }
}

/// The rest of `Handler`, passed through (each ends an emoji sequence).
macro_rules! forward {
    ($(fn $name:ident($($arg:ident: $ty:ty),*);)*) => {
        $(fn $name(&mut self, $($arg: $ty),*) {
            *self.last = None;
            self.term.$name($($arg),*)
        })*
    };
}

impl<T: EventListener> Handler for Graphemes<'_, T> {
    fn input(&mut self, c: char) {
        let last = self.last.replace(c);
        let joined = match c {
            VS16 => last.is_some_and(|p| crate::editor::width(p) == 1) && self.widen(),
            '\u{1f3fb}'..='\u{1f3ff}' => {
                last.is_some() && self.previous_is_wide() && {
                    self.attach(c);
                    true
                }
            }
            _ => {
                last == Some(ZWJ) && self.previous_is_wide() && {
                    self.attach(c);
                    true
                }
            }
        };
        if !joined {
            self.term.input(c);
        }
    }

    forward! {
        fn set_title(a0: Option<String>);
        fn set_cursor_style(a0: Option<CursorStyle>);
        fn set_cursor_shape(a0: CursorShape);
        fn goto(a0: i32, a1: usize);
        fn goto_line(a0: i32);
        fn goto_col(a0: usize);
        fn insert_blank(a0: usize);
        fn move_up(a0: usize);
        fn move_down(a0: usize);
        fn identify_terminal(a0: Option<char>);
        fn device_status(a0: usize);
        fn move_forward(a0: usize);
        fn move_backward(a0: usize);
        fn move_down_and_cr(a0: usize);
        fn move_up_and_cr(a0: usize);
        fn put_tab(a0: u16);
        fn backspace();
        fn carriage_return();
        fn linefeed();
        fn bell();
        fn substitute();
        fn newline();
        fn set_horizontal_tabstop();
        fn scroll_up(a0: usize);
        fn scroll_down(a0: usize);
        fn insert_blank_lines(a0: usize);
        fn delete_lines(a0: usize);
        fn erase_chars(a0: usize);
        fn delete_chars(a0: usize);
        fn move_backward_tabs(a0: u16);
        fn move_forward_tabs(a0: u16);
        fn save_cursor_position();
        fn restore_cursor_position();
        fn clear_line(a0: LineClearMode);
        fn clear_screen(a0: ClearMode);
        fn clear_tabs(a0: TabulationClearMode);
        fn set_tabs(a0: u16);
        fn reset_state();
        fn reverse_index();
        fn terminal_attribute(a0: Attr);
        fn set_mode(a0: Mode);
        fn unset_mode(a0: Mode);
        fn report_mode(a0: Mode);
        fn set_private_mode(a0: PrivateMode);
        fn unset_private_mode(a0: PrivateMode);
        fn report_private_mode(a0: PrivateMode);
        fn set_scrolling_region(a0: usize, a1: Option<usize>);
        fn set_keypad_application_mode();
        fn unset_keypad_application_mode();
        fn set_active_charset(a0: CharsetIndex);
        fn configure_charset(a0: CharsetIndex, a1: StandardCharset);
        fn set_color(a0: usize, a1: Rgb);
        fn dynamic_color_sequence(a0: String, a1: usize, a2: &str);
        fn reset_color(a0: usize);
        fn clipboard_store(a0: u8, a1: &[u8]);
        fn clipboard_load(a0: u8, a1: &str);
        fn decaln();
        fn push_title();
        fn pop_title();
        fn text_area_size_pixels();
        fn text_area_size_chars();
        fn set_hyperlink(a0: Option<Hyperlink>);
        fn set_mouse_cursor_icon(a0: CursorIcon);
        fn report_keyboard_mode();
        fn push_keyboard_mode(a0: KeyboardModes);
        fn pop_keyboard_modes(a0: u16);
        fn set_keyboard_mode(a0: KeyboardModes, a1: KeyboardModesApplyBehavior);
        fn set_modify_other_keys(a0: ModifyOtherKeys);
        fn report_modify_other_keys();
        fn set_scp(a0: ScpCharPath, a1: ScpUpdateMode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::Config;

    /// The cursor's column after printing `text` from column 0.
    fn column_after(text: &str) -> usize {
        let mut term = Term::new(Config::default(), &TermSize::new(20, 2), VoidListener);
        let mut last = None;
        let mut parser = Processor::<StdSyncHandler>::new();
        parser.advance(&mut Graphemes { term: &mut term, last: &mut last }, text.as_bytes());
        term.grid().cursor.point.column.0
    }

    #[test]
    fn emoji_sequences_take_two_columns() {
        assert_eq!(column_after("a"), 1);
        assert_eq!(column_after("👌"), 2);
        assert_eq!(column_after("❤️x"), 3);
        assert_eq!(column_after("👍🏽x"), 3);
        assert_eq!(column_after("👨‍👩‍👧x"), 3);
        assert_eq!(column_after("👨‍👩‍👧‍👦 ❤️ 👍🏽."), 9);
        // A skin tone on its own, or after a narrow character, still takes room.
        assert_eq!(column_after("a🏽"), 3);
    }

    #[test]
    fn joined_emoji_share_a_cell() {
        let mut term = Term::new(Config::default(), &TermSize::new(20, 2), VoidListener);
        let mut last = None;
        let mut parser = Processor::<StdSyncHandler>::new();
        parser.advance(&mut Graphemes { term: &mut term, last: &mut last }, "❤️👨‍👩‍👧".as_bytes());
        let row = &term.grid()[Line(0)];
        assert!(row[Column(0)].flags.contains(Flags::WIDE_CHAR));
        assert_eq!(row[Column(0)].zerowidth(), Some(&[VS16][..]));
        assert_eq!(row[Column(2)].c, '👨');
        assert_eq!(row[Column(2)].zerowidth(), Some(&[ZWJ, '👩', ZWJ, '👧'][..]));
    }
}

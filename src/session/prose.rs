use super::*;

impl Block {
    /// Shows `text` as the output, its lines broken between words; kept, to
    /// break them again when the width changes.
    pub(super) fn set_prose(&mut self, text: String) {
        self.prose = Some(text);
        self.lay_prose();
    }

    /// Writes the prose out again from nothing, at the width as it is now.
    pub(super) fn lay_prose(&mut self) {
        let Some(prose) = &self.prose else { return };
        let wrapped = wrap_words(prose, self.size.columns).replace('\n', "\r\n");
        self.last = None;
        // A full reset first: the screen and the lines scrolled off it.
        self.feed(format!("\x1bc{wrapped}").as_bytes());
    }
}

/// `text` with a line break put in before any word that would run past
/// `columns`, taking the place of the space before it. A word longer than
/// a whole line starts one, and is left to wrap where it must.
pub(super) fn wrap_words(text: &str, columns: usize) -> String {
    let width = |s: &str| s.chars().map(char_width).sum::<usize>();
    let mut out = String::with_capacity(text.len() + text.len() / 40);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut used = 0;
        let mut rest = line;
        while !rest.is_empty() {
            // The spaces before a word, then the word.
            let gap = rest.len() - rest.trim_start_matches([' ', '\t']).len();
            let word = rest[gap..].find([' ', '\t']).map_or(rest.len(), |end| gap + end);
            let (space, text) = (&rest[..gap], &rest[gap..word]);
            rest = &rest[word..];
            let (space_w, text_w) = (width(space), width(text));
            if used > 0 && !text.is_empty() && used + space_w + text_w > columns {
                out.push('\n');
                used = 0;
            } else {
                out.push_str(space);
                used += space_w;
            }
            out.push_str(text);
            // Past the edge, the terminal has wrapped it.
            used = if columns > 0 && used + text_w > columns { (used + text_w) % columns } else { used + text_w };
        }
    }
    out
}

fn char_width(c: char) -> usize {
    if c == '\t' {
        8
    } else {
        unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_break_between_words() {
        assert_eq!(wrap_words("the fish shell. The end", 17), "the fish shell.\nThe end");
        // Exactly fitting stays; one more column breaks.
        assert_eq!(wrap_words("ab cd", 5), "ab cd");
        assert_eq!(wrap_words("ab cd", 4), "ab\ncd");
        // The text's own line breaks and indents stay.
        assert_eq!(wrap_words("one\n  ls -l  x\n\nend", 20), "one\n  ls -l  x\n\nend");
        // Wide characters count two columns.
        assert_eq!(wrap_words("ab 你好", 6), "ab\n你好");
    }

    #[test]
    fn a_word_longer_than_a_line_starts_one() {
        // It ends 1 column into its third line, so " now" fits after it.
        assert_eq!(wrap_words("see /a/very/long/path now", 8), "see\n/a/very/long/path now");
        // What is left of it on its last line counts: 2 columns, then " ok".
        assert_eq!(wrap_words("abcdefghij ok", 8), "abcdefghij ok");
        assert_eq!(wrap_words("abcdefghijklmn ok", 8), "abcdefghijklmn\nok");
    }

    #[test]
    fn prose_is_broken_again_when_the_width_changes() {
        let listener = Listener(Rc::new(RefCell::new(Vec::new())));
        let size = GridSize { columns: 12, lines: 4 };
        let mut block = Block::new(1, BlockKind::Ai, "ask".into(), "/".into(), None, size, listener);
        block.set_prose("alpha beta gamma delta epsilon zeta eta theta".into());
        assert_eq!(block.output_text(), "alpha beta\ngamma delta\nepsilon zeta\neta theta");
        // More lines than the screen: the reset must clear what scrolled off too.
        block.resize(GridSize { columns: 6, lines: 4 });
        assert_eq!(block.output_lines(), 8);
        block.resize(GridSize { columns: 40, lines: 4 });
        assert_eq!(block.output_text(), "alpha beta gamma delta epsilon zeta eta\ntheta");
    }
}

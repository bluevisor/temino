//! The command editor at the bottom of the window (Warp-style: the shell
//! never sees a keystroke until Enter), with history and autosuggestions.

pub struct Editor {
    pub text: String,
    /// Byte offset of the caret.
    pub cursor: usize,
    /// Byte offset of the selection's other end.
    pub anchor: Option<usize>,
    undo: Vec<(String, usize)>,
    /// Position in history while browsing with Up/Down, and the draft.
    browsing: Option<(usize, String)>,
    /// The column Up/Down aims for, and where the caret was when it last
    /// moved that way: passing a shorter line doesn't lose the column.
    goal: Option<(usize, usize)>,
}

/// A character that belongs to the one before it on screen: a combining
/// mark, a variation selector (❤️), a skin tone (👍🏽), a joiner (👨‍👩‍👧), a
/// keycap (1️⃣) or a flag tag.
fn extends(c: char) -> bool {
    matches!(c, '\u{200c}' | '\u{200d}' | '\u{fe00}'..='\u{fe0f}' | '\u{20e3}' | '\u{1f3fb}'..='\u{1f3ff}' | '\u{e0020}'..='\u{e007f}')
        || (!c.is_control() && unicode_width::UnicodeWidthChar::width(c) == Some(0))
}

fn is_regional(c: char) -> bool {
    ('\u{1f1e6}'..='\u{1f1ff}').contains(&c)
}

/// Whether `c` stays on screen as one character with the `before` it.
fn joins(before: char, c: char) -> bool {
    extends(c) || before == '\u{200d}'
}

/// The start of the character the caret is after, a whole emoji sequence
/// or accented letter at once, so Backspace and Left never split one.
fn prev_boundary(text: &str, at: usize) -> usize {
    let mut chars = text[..at].char_indices().rev().peekable();
    let Some((mut start, mut c)) = chars.next() else { return 0 };
    if is_regional(c) {
        // Flags are pairs of letters: 🇺🇸 is U then S.
        let run = text[..start].chars().rev().take_while(|&c| is_regional(c)).count();
        if run % 2 == 1 {
            return prev_boundary_char(text, start);
        }
        return start;
    }
    while let Some(&(i, before)) = chars.peek() {
        if !joins(before, c) || before == '\n' {
            break;
        }
        (start, c) = (i, before);
        chars.next();
    }
    start
}

fn prev_boundary_char(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

/// The end of the character the caret is before (see `prev_boundary`).
fn next_boundary(text: &str, at: usize) -> usize {
    let mut chars = text[at..].chars();
    let Some(mut c) = chars.next() else { return at };
    let mut end = at + c.len_utf8();
    if is_regional(c) {
        // Flags pair from the start of the run of letters.
        let run = text[..at].chars().rev().take_while(|&c| is_regional(c)).count();
        match chars.next() {
            Some(next) if run % 2 == 0 && is_regional(next) => return end + next.len_utf8(),
            _ => return end,
        }
    }
    for next in chars {
        if c == '\n' || !joins(c, next) {
            break;
        }
        end += next.len_utf8();
        c = next;
    }
    end
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Editor {
    pub fn new() -> Editor {
        Editor { text: String::new(), cursor: 0, anchor: None, undo: Vec::new(), browsing: None, goal: None }
    }

    fn snapshot(&mut self) {
        if self.undo.last().map(|(t, _)| t != &self.text).unwrap_or(true) {
            self.undo.push((self.text.clone(), self.cursor));
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.browsing = None;
    }

    pub fn undo(&mut self) {
        if let Some((text, cursor)) = self.undo.pop() {
            self.text = text;
            self.cursor = cursor.min(self.text.len());
            self.anchor = None;
        }
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
        self.anchor = None;
    }

    pub fn clear(&mut self) {
        self.snapshot();
        self.set("");
    }

    pub fn take(&mut self) -> String {
        self.browsing = None;
        self.undo.clear();
        self.anchor = None;
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| (anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.text[a..b])
    }

    fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selection() {
            self.text.replace_range(a..b, "");
            self.cursor = a;
            self.anchor = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }

    pub fn insert(&mut self, s: &str) {
        self.snapshot();
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        self.snapshot();
        if !self.delete_selection() && self.cursor > 0 {
            let start = prev_boundary(&self.text, self.cursor);
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
    }

    pub fn delete(&mut self) {
        self.snapshot();
        if !self.delete_selection() && self.cursor < self.text.len() {
            let end = next_boundary(&self.text, self.cursor);
            self.text.replace_range(self.cursor..end, "");
        }
    }

    fn word_left(&self, from: usize) -> usize {
        let mut at = from;
        while at > 0 && !is_word(self.text[..at].chars().next_back().unwrap()) {
            at = prev_boundary(&self.text, at);
        }
        while at > 0 && is_word(self.text[..at].chars().next_back().unwrap()) {
            at = prev_boundary(&self.text, at);
        }
        at
    }

    fn word_right(&self, from: usize) -> usize {
        let mut at = from;
        while at < self.text.len() && !is_word(self.text[at..].chars().next().unwrap()) {
            at = next_boundary(&self.text, at);
        }
        while at < self.text.len() && is_word(self.text[at..].chars().next().unwrap()) {
            at = next_boundary(&self.text, at);
        }
        at
    }

    pub fn delete_word_back(&mut self) {
        self.snapshot();
        if self.delete_selection() {
            return;
        }
        let start = self.word_left(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn kill_to_start(&mut self) {
        self.snapshot();
        let start = self.line_start(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.anchor = None;
    }

    pub fn kill_to_end(&mut self) {
        self.snapshot();
        let end = self.line_end(self.cursor);
        self.text.replace_range(self.cursor..end, "");
        self.anchor = None;
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..].find('\n').map_or(self.text.len(), |i| at + i)
    }

    fn place(&mut self, to: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = to;
    }

    pub fn left(&mut self, word: bool, select: bool) {
        let to = match (self.selection(), select, word) {
            (Some((a, _)), false, false) => a,
            _ if word => self.word_left(self.cursor),
            _ => prev_boundary(&self.text, self.cursor),
        };
        self.place(to, select);
    }

    pub fn right(&mut self, word: bool, select: bool) {
        let to = match (self.selection(), select, word) {
            (Some((_, b)), false, false) => b,
            _ if word => self.word_right(self.cursor),
            _ => next_boundary(&self.text, self.cursor),
        };
        self.place(to, select);
    }

    pub fn home(&mut self, select: bool) {
        let to = self.line_start(self.cursor);
        self.place(to, select);
    }

    pub fn end(&mut self, select: bool) {
        let to = self.line_end(self.cursor);
        self.place(to, select);
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Selects the word under the caret (a double click), or the run of
    /// spaces or of punctuation there.
    pub fn select_word(&mut self) {
        let kind = |c: char| {
            if is_word(c) {
                0
            } else if c.is_whitespace() {
                1
            } else {
                2
            }
        };
        // At a line's end the click was on its last character.
        let Some(c) = self.text[self.cursor..]
            .chars()
            .next()
            .filter(|&c| c != '\n')
            .or_else(|| self.text[..self.cursor].chars().next_back())
        else {
            return;
        };
        let k = kind(c);
        let start = self.text[..self.cursor]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| kind(c) == k && c != '\n')
            .last()
            .map_or(self.cursor, |(i, _)| i);
        let end = self.text[self.cursor..]
            .char_indices()
            .find(|&(_, c)| kind(c) != k || c == '\n')
            .map_or(self.text.len(), |(i, _)| self.cursor + i);
        self.anchor = Some(start);
        self.cursor = end;
    }

    /// Selects the caret's line, without its newline (a triple click).
    pub fn select_line(&mut self) {
        self.anchor = Some(self.line_start(self.cursor));
        self.cursor = self.line_end(self.cursor);
    }

    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    /// (line, column in cells) of a byte offset: CJK takes two cells.
    pub fn position(&self, at: usize) -> (usize, usize) {
        let line = self.text[..at].matches('\n').count();
        let start = self.line_start(at);
        (line, columns(&self.text[start..at]))
    }

    /// Byte offset of (line, column in cells), clamped; a column inside a
    /// wide character lands before it.
    pub fn offset(&self, line: usize, column: usize) -> usize {
        let mut start = 0;
        for _ in 0..line {
            match self.text[start..].find('\n') {
                Some(i) => start += i + 1,
                None => return self.text.len(),
            }
        }
        let end = self.line_end(start);
        let (mut at, mut cells) = (start, 0);
        while at < end {
            // A whole emoji sequence at a time, so the caret never lands inside one.
            let next = next_boundary(&self.text, at).min(end);
            cells += columns(&self.text[at..next]);
            if cells > column {
                return at;
            }
            at = next;
        }
        end
    }

    /// Moves between lines; returns false on the first/last line so the
    /// caller can browse history instead.
    pub fn vertical(&mut self, down: bool, select: bool) -> bool {
        let (line, here) = self.position(self.cursor);
        let lines = self.line_count();
        if (down && line + 1 >= lines) || (!down && line == 0) {
            return false;
        }
        // Still where the last Up/Down left it: aim for the same column.
        let column = match self.goal {
            Some((at, column)) if at == self.cursor => column,
            _ => here,
        };
        let target = if down { line + 1 } else { line - 1 };
        let to = self.offset(target, column);
        self.place(to, select);
        self.goal = Some((to, column));
        true
    }

    /// Up/Down through history; the current text is kept as a draft.
    pub fn browse(&mut self, history: &History, older: bool) -> bool {
        let entries = &history.entries;
        let next = match (&self.browsing, older) {
            (None, true) => entries.len().checked_sub(1),
            (None, false) => None,
            (Some((i, _)), true) => i.checked_sub(1),
            (Some((i, _)), false) => Some(i + 1),
        };
        match next {
            Some(i) if i < entries.len() => {
                let draft = self.browsing.take().map(|(_, d)| d).unwrap_or_else(|| self.text.clone());
                self.set(&entries[i].clone());
                self.browsing = Some((i, draft));
                true
            }
            Some(_) => {
                if let Some((_, draft)) = self.browsing.take() {
                    self.set(&draft);
                }
                true
            }
            None => older && self.browsing.is_some(),
        }
    }

    /// The rest of the newest history entry starting with the text, shown
    /// as ghost text after the caret (fish/Warp-style).
    pub fn suggestion<'a>(&self, history: &'a History) -> Option<&'a str> {
        if self.text.trim().is_empty() || self.cursor != self.text.len() || self.text.contains('\n') {
            return None;
        }
        history
            .entries
            .iter()
            .rev()
            .find(|entry| entry.len() > self.text.len() && entry.starts_with(&self.text) && !entry.contains('\n'))
            .map(|entry| &entry[self.text.len()..])
    }
}

/// Reverses `History::save`'s escaping (`\\` and `\n`).
fn unescape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

pub struct History {
    pub entries: Vec<String>,
    path: String,
}

impl History {
    const LIMIT: usize = 5000;

    /// Temino's own history, seeded from ~/.bash_history the first time.
    pub fn load() -> History {
        let path = format!("{}/history", crate::settings_dir());
        let mut entries: Vec<String> = match std::fs::read_to_string(&path) {
            Ok(text) => text.lines().map(unescape).collect(),
            Err(_) => {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/boot/home".into());
                std::fs::read_to_string(format!("{home}/.bash_history"))
                    .map(|text| text.lines().filter(|l| !l.starts_with('#')).map(str::to_owned).collect())
                    .unwrap_or_default()
            }
        };
        let mut seen = std::collections::HashSet::new();
        entries.reverse();
        entries.retain(|entry| !entry.trim().is_empty() && seen.insert(entry.clone()));
        entries.truncate(Self::LIMIT);
        entries.reverse();
        History { entries, path }
    }

    pub fn push(&mut self, entry: &str) {
        let entry = entry.trim_end();
        if entry.trim().is_empty() || entry.starts_with(' ') {
            return;
        }
        self.entries.retain(|e| e != entry);
        self.entries.push(entry.to_owned());
        if self.entries.len() > Self::LIMIT {
            self.entries.remove(0);
        }
        self.save();
    }

    fn save(&self) {
        let text: String = self.entries.iter().map(|e| e.replace('\\', "\\\\").replace('\n', "\\n") + "\n").collect();
        crate::create_private_dir(&crate::settings_dir());
        crate::write_private(&self.path, text);
    }
}

/// Cells a character takes on screen: 2 for CJK and other wide forms.
pub fn width(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(0).max(if c == '\t' { 1 } else { 0 })
}

/// Cells a line of text takes.
pub fn columns(text: &str) -> usize {
    text.chars().map(width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_click_selects_the_word_or_run_under_the_caret() {
        let mut editor = Editor::new();
        editor.insert("echo can't  select_this\nnext");
        let select_at = |editor: &mut Editor, at: usize| {
            editor.cursor = at;
            editor.select_word();
            editor.selected_text().unwrap_or("").to_owned()
        };
        assert_eq!(select_at(&mut editor, 0), "echo");
        assert_eq!(select_at(&mut editor, 2), "echo");
        assert_eq!(select_at(&mut editor, 5), "can");
        assert_eq!(select_at(&mut editor, 8), "'");
        assert_eq!(select_at(&mut editor, 10), "  ");
        assert_eq!(select_at(&mut editor, 14), "select_this");
        // at the end of a line: the word before the caret, not the newline
        assert_eq!(select_at(&mut editor, 23), "select_this");
        let end = editor.text.len();
        assert_eq!(select_at(&mut editor, end), "next");
    }

    #[test]
    fn triple_click_selects_the_line_without_its_newline() {
        let mut editor = Editor::new();
        editor.insert("first line\nsecond");
        editor.cursor = 3;
        editor.select_line();
        assert_eq!(editor.selected_text(), Some("first line"));
        editor.cursor = editor.text.len() - 1;
        editor.select_line();
        assert_eq!(editor.selected_text(), Some("second"));
    }

    #[test]
    fn wide_characters_take_two_columns() {
        let mut editor = Editor::new();
        editor.insert("ls 中文a");
        assert_eq!(editor.position(editor.text.len()), (0, 8));
        // 中 covers columns 3 and 4: either lands before it.
        assert_eq!(editor.offset(0, 3), 3);
        assert_eq!(editor.offset(0, 4), 3);
        assert_eq!(editor.offset(0, 5), "ls 中".len());
        assert_eq!(editor.offset(0, 7), "ls 中文".len());
    }
    fn typed(text: &str) -> Editor {
        let mut editor = Editor::new();
        editor.insert(text);
        editor
    }

    fn past(entries: &[&str]) -> History {
        History { entries: entries.iter().map(|e| e.to_string()).collect(), path: String::new() }
    }

    #[test]
    fn word_motions_skip_punctuation_then_a_word() {
        let mut editor = typed("git commit -m 'fix bug'");
        editor.cursor = 0;
        let mut stops = Vec::new();
        while editor.cursor < editor.text.len() {
            editor.right(true, false);
            stops.push(editor.cursor);
        }
        assert_eq!(stops, [3, 10, 13, 18, 22, 23]);
        let mut stops = Vec::new();
        while editor.cursor > 0 {
            editor.left(true, false);
            stops.push(editor.cursor);
        }
        assert_eq!(stops, [19, 15, 12, 4, 0]);
        // Underscores and digits are part of a word.
        let mut editor = typed("my_var2 = 1");
        editor.cursor = 0;
        editor.right(true, false);
        assert_eq!(editor.cursor, 7);
    }

    #[test]
    fn word_motions_treat_non_ascii_letters_as_word_characters() {
        let mut editor = typed("ls 中文 über");
        editor.cursor = 0;
        editor.right(true, false);
        assert_eq!(editor.cursor, 2);
        editor.right(true, false);
        assert_eq!(editor.cursor, "ls 中文".len());
        editor.right(true, false);
        assert_eq!(editor.cursor, editor.text.len());
        editor.left(true, false);
        assert_eq!(editor.cursor, "ls 中文 ".len());
    }

    #[test]
    fn arrows_and_deletes_step_over_whole_characters() {
        let mut editor = typed("a😀中b");
        editor.left(false, false);
        assert_eq!(editor.cursor, "a😀中".len());
        editor.left(false, false);
        assert_eq!(editor.cursor, "a😀".len());
        editor.left(false, false);
        assert_eq!(editor.cursor, 1);
        editor.right(false, false);
        assert_eq!(editor.cursor, "a😀".len());
        editor.backspace();
        assert_eq!((editor.text.as_str(), editor.cursor), ("a中b", 1));
        editor.delete();
        assert_eq!((editor.text.as_str(), editor.cursor), ("ab", 1));
        // At either end, nothing happens and nothing panics.
        editor.cursor = 0;
        editor.backspace();
        editor.left(false, false);
        editor.left(true, false);
        assert_eq!((editor.text.as_str(), editor.cursor), ("ab", 0));
        editor.cursor = 2;
        editor.delete();
        editor.right(false, false);
        editor.right(true, false);
        assert_eq!((editor.text.as_str(), editor.cursor), ("ab", 2));
    }

    #[test]
    fn backspace_removes_a_whole_emoji() {
        let mut editor = typed("ok 👍🏽");
        editor.backspace();
        assert_eq!(editor.text, "ok ");
        let mut editor = typed("👨‍👩‍👧");
        editor.backspace();
        assert_eq!(editor.text, "");
    }

    #[test]
    fn the_caret_steps_over_whole_characters() {
        // Delete, Left and Right treat each of these as one character.
        for each in ["👍🏽", "👨‍👩‍👧", "🇺🇸", "e\u{301}", "❤️", "1️⃣"] {
            let mut editor = typed(&format!("a{each}b"));
            editor.left(false, false);
            editor.left(false, false);
            assert_eq!(editor.cursor, 1, "{each}");
            editor.right(false, false);
            assert_eq!(editor.cursor, 1 + each.len(), "{each}");
            editor.cursor = 1;
            editor.delete();
            assert_eq!(editor.text, "ab", "{each}");
        }
        // Two flags side by side are two characters.
        let mut editor = typed("🇺🇸🇫🇷");
        editor.backspace();
        assert_eq!(editor.text, "🇺🇸");
        // A joiner never reaches across a line break.
        let mut editor = typed("a\n\u{301}");
        editor.backspace();
        assert_eq!(editor.text, "a\n");
    }

    #[test]
    fn up_and_down_remember_the_column_past_a_short_line() {
        let mut editor = typed("abcdef\nab\nabcdef");
        editor.cursor = 5;
        assert!(editor.vertical(true, false));
        assert_eq!(editor.cursor, "abcdef\nab".len());
        assert!(editor.vertical(true, false));
        assert_eq!(editor.cursor, "abcdef\nab\nabcde".len());
        assert!(editor.vertical(false, false));
        assert!(editor.vertical(false, false));
        assert_eq!(editor.cursor, 5);
        // Moving any other way sets a new column.
        editor.cursor = 1;
        assert!(editor.vertical(true, false));
        assert_eq!(editor.cursor, "abcdef\na".len());
    }

    #[test]
    fn deleting_a_word_back() {
        let mut editor = typed("cd ~/projects/temino");
        editor.delete_word_back();
        assert_eq!(editor.text, "cd ~/projects/");
        editor.delete_word_back();
        assert_eq!(editor.text, "cd ~/");
        editor.delete_word_back();
        assert_eq!(editor.text, "");
        let mut editor = typed("echo hi  ");
        editor.delete_word_back();
        assert_eq!(editor.text, "echo ");
        // Mid-line: only what is before the caret goes.
        let mut editor = typed("one two three");
        editor.cursor = 7;
        editor.delete_word_back();
        assert_eq!((editor.text.as_str(), editor.cursor), ("one  three", 4));
    }

    #[test]
    fn shift_arrows_select_and_typing_replaces_the_selection() {
        let mut editor = typed("hello");
        editor.left(false, true);
        editor.left(false, true);
        assert_eq!(editor.selected_text(), Some("lo"));
        assert_eq!(editor.selection(), Some((3, 5)));
        editor.insert("p");
        assert_eq!((editor.text.as_str(), editor.cursor, editor.selection()), ("help", 4, None));
        // Word selection, then Backspace deletes just the selection.
        let mut editor = typed("rm -rf build");
        editor.left(true, true);
        assert_eq!(editor.selected_text(), Some("build"));
        editor.backspace();
        assert_eq!(editor.text, "rm -rf ");
        // Forward selection, then Delete.
        let mut editor = typed("abc");
        editor.cursor = 0;
        editor.right(false, true);
        editor.delete();
        assert_eq!(editor.text, "bc");
        // Selecting back to where it started selects nothing.
        let mut editor = typed("abc");
        editor.left(false, true);
        editor.right(false, true);
        assert_eq!(editor.selection(), None);
    }

    #[test]
    fn plain_arrows_collapse_a_selection_to_its_edge() {
        let mut editor = typed("one two three");
        editor.cursor = 4;
        editor.right(true, true);
        assert_eq!(editor.selected_text(), Some("two"));
        editor.left(false, false);
        assert_eq!((editor.cursor, editor.selection()), (4, None));
        editor.right(true, true);
        editor.right(false, false);
        assert_eq!((editor.cursor, editor.selection()), (7, None));
        // Home and End drop the selection unless Shift is held.
        editor.home(true);
        assert_eq!(editor.selected_text(), Some("one two"));
        editor.end(false);
        assert_eq!((editor.cursor, editor.selection()), (13, None));
    }

    #[test]
    fn select_all_then_typing_replaces_everything() {
        let mut editor = typed("old\ncommand");
        editor.select_all();
        assert_eq!(editor.selected_text(), Some("old\ncommand"));
        editor.insert("new");
        assert_eq!((editor.text.as_str(), editor.cursor), ("new", 3));
        let mut empty = Editor::new();
        empty.select_all();
        assert_eq!(empty.selected_text(), None);
    }

    #[test]
    fn undo_steps_back_through_edits_with_the_caret() {
        let mut editor = Editor::new();
        editor.insert("ls");
        editor.insert(" -la");
        editor.cursor = 2;
        editor.delete_word_back();
        assert_eq!(editor.text, " -la");
        editor.undo();
        assert_eq!((editor.text.as_str(), editor.cursor), ("ls -la", 2));
        editor.undo();
        assert_eq!((editor.text.as_str(), editor.cursor), ("ls", 2));
        editor.undo();
        assert_eq!((editor.text.as_str(), editor.cursor), ("", 0));
        // Nothing left: undo does nothing.
        editor.undo();
        assert_eq!(editor.text, "");
        // Undo drops a selection and brings back replaced text.
        let mut editor = typed("abc");
        editor.select_all();
        editor.insert("x");
        editor.undo();
        assert_eq!((editor.text.as_str(), editor.selection()), ("abc", None));
        // Clearing is undoable too.
        editor.clear();
        assert_eq!(editor.text, "");
        editor.undo();
        assert_eq!(editor.text, "abc");
    }

    #[test]
    fn take_empties_the_editor_and_forgets_its_undo() {
        let mut editor = typed("make test");
        assert_eq!(editor.take(), "make test");
        assert_eq!((editor.text.as_str(), editor.cursor, editor.anchor), ("", 0, None));
        editor.undo();
        assert_eq!(editor.text, "");
    }

    #[test]
    fn kills_and_home_end_stay_on_the_caret_line() {
        let mut editor = typed("first\nsecond line\nthird");
        editor.cursor = "first\nsecond".len();
        editor.home(false);
        assert_eq!(editor.cursor, "first\n".len());
        editor.end(false);
        assert_eq!(editor.cursor, "first\nsecond line".len());
        editor.cursor = "first\nsecond".len();
        editor.kill_to_end();
        assert_eq!(editor.text, "first\nsecond\nthird");
        editor.kill_to_start();
        assert_eq!((editor.text.as_str(), editor.cursor), ("first\n\nthird", "first\n".len()));
        // At a line's start, killing back does not join the lines.
        editor.kill_to_start();
        assert_eq!(editor.text, "first\n\nthird");
    }

    #[test]
    fn up_and_down_move_between_lines_keeping_the_column() {
        let mut editor = typed("abcdef\nab\n中文xyz");
        editor.cursor = 5;
        assert!(editor.vertical(true, false));
        assert_eq!(editor.cursor, "abcdef\nab".len());
        assert!(editor.vertical(true, false));
        // Still aiming for column 5, which falls after 中文x (2+2+1 cells).
        assert_eq!(editor.cursor, "abcdef\nab\n中文x".len());
        assert!(!editor.vertical(true, false));
        assert!(editor.vertical(false, true));
        assert!(editor.vertical(false, true));
        assert_eq!(editor.selected_text(), Some("f\nab\n中文x"));
        // Column 3 is inside 文, so the caret lands before it.
        editor.cursor = 3;
        assert!(editor.vertical(true, false));
        assert!(editor.vertical(true, false));
        assert_eq!(editor.cursor, "abcdef\nab\n中".len());
        editor.cursor = 2;
        assert!(!editor.vertical(false, false));
        assert_eq!(editor.line_count(), 3);
        assert_eq!(editor.position(editor.text.len()), (2, 7));
        assert_eq!(editor.offset(9, 0), editor.text.len());
        assert_eq!(editor.offset(1, 99), "abcdef\nab".len());
    }

    #[test]
    fn history_browsing_keeps_the_draft() {
        let history = past(&["ls", "pwd"]);
        let mut editor = typed("dra");
        assert!(editor.browse(&history, true));
        assert_eq!(editor.text, "pwd");
        assert!(editor.browse(&history, true));
        assert_eq!(editor.text, "ls");
        // Past the oldest: stays there, but the key is still used.
        assert!(editor.browse(&history, true));
        assert_eq!(editor.text, "ls");
        assert!(editor.browse(&history, false));
        assert_eq!(editor.text, "pwd");
        assert!(editor.browse(&history, false));
        assert_eq!((editor.text.as_str(), editor.cursor), ("dra", 3));
        // Not browsing: Down is not ours.
        assert!(!editor.browse(&history, false));
        assert!(!Editor::new().browse(&past(&[]), true));
    }

    #[test]
    fn editing_a_recalled_entry_stops_browsing() {
        let history = past(&["one", "two"]);
        let mut editor = Editor::new();
        editor.browse(&history, true);
        editor.insert("!");
        assert_eq!(editor.text, "two!");
        // Up starts again from the newest entry, with the edit as the draft.
        editor.browse(&history, true);
        assert_eq!(editor.text, "two");
        editor.browse(&history, false);
        assert_eq!(editor.text, "two!");
    }

    #[test]
    fn suggestions_complete_from_the_newest_matching_entry() {
        let history = past(&["git status", "git stash pop", "git st\nmulti", "echo hi"]);
        let at = |text: &str| typed(text).suggestion(&history).map(str::to_owned);
        assert_eq!(at("git st").as_deref(), Some("ash pop"));
        assert_eq!(at("git stat").as_deref(), Some("us"));
        assert_eq!(at("git status"), None);
        assert_eq!(at("nothing"), None);
        assert_eq!(at("  "), None);
        assert_eq!(at("a\nb"), None);
        // Only with the caret at the end.
        let mut editor = typed("git st");
        editor.cursor = 2;
        assert_eq!(editor.suggestion(&history), None);
    }

    #[test]
    fn history_file_escapes_come_back_exactly() {
        assert_eq!(unescape("plain"), "plain");
        assert_eq!(unescape(r"for x in a\nb"), "for x in a\nb");
        assert_eq!(unescape(r"echo \\n"), r"echo \n");
        assert_eq!(unescape(r"trailing\"), r"trailing\");
        assert_eq!(unescape(r"\t"), "t");
        // What save writes, unescape reads back.
        for entry in ["a\\b", "line\nnext", "\\n literal", "中文\\", ""] {
            let saved = entry.replace('\\', "\\\\").replace('\n', "\\n");
            assert_eq!(unescape(&saved), entry);
        }
    }

    #[test]
    fn character_widths() {
        assert_eq!(width('a'), 1);
        assert_eq!(width('中'), 2);
        assert_eq!(width('😀'), 2);
        assert_eq!(width('\t'), 1);
        assert_eq!(width('\u{0301}'), 0);
        assert_eq!(width('\x07'), 0);
        assert_eq!(columns("ls 中文"), 7);
        assert_eq!(columns(""), 0);
    }
}

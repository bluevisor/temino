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
}

fn prev_boundary(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn next_boundary(text: &str, at: usize) -> usize {
    text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Editor {
    pub fn new() -> Editor {
        Editor { text: String::new(), cursor: 0, anchor: None, undo: Vec::new(), browsing: None }
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
        let mut cells = 0;
        for (i, c) in self.text[start..end].char_indices() {
            cells += width(c);
            if cells > column {
                return start + i;
            }
        }
        end
    }

    /// Moves between lines; returns false on the first/last line so the
    /// caller can browse history instead.
    pub fn vertical(&mut self, down: bool, select: bool) -> bool {
        let (line, column) = self.position(self.cursor);
        let lines = self.line_count();
        if (down && line + 1 >= lines) || (!down && line == 0) {
            return false;
        }
        let target = if down { line + 1 } else { line - 1 };
        let to = self.offset(target, column);
        self.place(to, select);
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
}

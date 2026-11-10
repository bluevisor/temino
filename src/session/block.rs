use super::*;

impl Block {
    pub(super) fn new(
        id: u64,
        kind: BlockKind,
        command: String,
        cwd: String,
        branch: Option<String>,
        size: GridSize,
        listener: Listener,
    ) -> Block {
        let config = Config { scrolling_history: HISTORY_LINES, ..Config::default() };
        let clears = kind == BlockKind::Command && matches!(command.trim(), "clear" | "reset" | "cls");
        Block {
            id,
            kind,
            command,
            cwd,
            branch,
            started: Instant::now(),
            finished: None,
            exit: None,
            term: Term::new(config, &size, listener),
            parser: Processor::new(),
            last: None,
            lines: None,
            size,
            clears,
            generation: 0,
            pictures: Vec::new(),
            prose: None,
            source: None,
        }
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) {
        let mut handler = crate::graphemes::Graphemes { term: &mut self.term, last: &mut self.last };
        self.parser.advance(&mut handler, bytes);
        self.lines = None;
        self.generation += 1;
    }

    pub fn running(&self) -> bool {
        self.finished.is_none()
    }

    pub fn failed(&self) -> bool {
        matches!(self.exit, Some(code) if code != 0)
    }

    pub fn duration(&self) -> Duration {
        self.finished.unwrap_or_else(Instant::now).duration_since(self.started)
    }

    pub fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Lines of output worth showing: scrollback plus the screen down to
    /// its last non-blank line (or the cursor, while running).
    pub fn output_lines(&mut self) -> usize {
        if let Some(lines) = self.lines {
            return lines;
        }
        let grid = self.term.grid();
        let screen = grid.screen_lines();
        let cursor = grid.cursor.point;
        let mut bottom: i32 = -1;
        for line in (0..screen).rev() {
            let row = &grid[Line(line as i32)];
            let blank = (0..grid.columns()).all(|c| {
                let cell = &row[Column(c)];
                (cell.c == ' ' || cell.c == '\t')
                    && cell.bg
                        == alacritty_terminal::vte::ansi::Color::Named(
                            alacritty_terminal::vte::ansi::NamedColor::Background,
                        )
                    && !cell.flags.intersects(Flags::INVERSE | Flags::ALL_UNDERLINES)
            });
            if !blank {
                bottom = line as i32;
                break;
            }
        }
        if self.running() && (cursor.column.0 > 0 || bottom >= 0) {
            bottom = bottom.max(cursor.line.0);
        }
        let pictures = self.pictures.iter().map(|p| p.line + p.rows).max().unwrap_or(0);
        let lines = (grid.history_size() + (bottom + 1).max(0) as usize).max(pictures);
        self.lines = Some(lines);
        lines
    }

    /// Puts an image at the cursor, sized to the program's request or else
    /// its own pixels (one per logical pixel, no wider than the screen),
    /// and moves the cursor below it, as iTerm2 and Kitty do.
    pub fn place_picture(&mut self, request: crate::picture::Request, cell: (f32, f32)) {
        if self.alt_screen() {
            return;
        }
        let picture = request.picture;
        let grid = self.term.grid();
        let point = grid.cursor.point;
        let room = grid.columns().saturating_sub(point.column.0).max(1);
        let aspect = picture.height as f32 / picture.width as f32;
        let (columns, rows) = match (request.size.columns, request.size.rows) {
            (Some(c), Some(r)) => (c.min(room), r),
            (Some(c), None) => {
                let c = c.min(room);
                (c, ((c as f32 * cell.0 * aspect) / cell.1).ceil() as usize)
            }
            (None, Some(r)) => {
                let c = ((r as f32 * cell.1 / aspect) / cell.0).ceil() as usize;
                (c.clamp(1, room), r)
            }
            (None, None) => {
                let c = ((picture.width as f32 / cell.0).ceil() as usize).clamp(1, room);
                (c, ((c as f32 * cell.0 * aspect) / cell.1).ceil() as usize)
            }
        };
        let rows = rows.clamp(1, 1000);
        let line = (grid.history_size() as i32 + point.line.0) as usize;
        self.pictures.push(Placement { picture, line, column: point.column.0, columns, rows });
        // The image's rows, then the cursor on the line under it.
        self.feed(&b"\r\n".repeat(rows));
    }

    /// The last `limit` lines of output as text with SGR colors, to be fed
    /// back in after a relaunch.
    pub fn output_ansi(&mut self, limit: usize) -> Vec<u8> {
        use alacritty_terminal::vte::ansi::{Color as C, NamedColor};
        let lines = self.output_lines();
        let grid = self.term.grid();
        let columns = grid.columns();
        let sgr = |color: C, foreground: bool| -> String {
            let base = if foreground { 30 } else { 40 };
            match color {
                C::Named(n) if (n as usize) < 8 => format!("{}", base + n as usize),
                C::Named(n) if (n as usize) < 16 => format!("{}", base + 60 + n as usize - 8),
                C::Named(NamedColor::Foreground | NamedColor::Background) | C::Named(_) => format!("{}", base + 9),
                C::Indexed(i) => format!("{};5;{i}", base + 8),
                C::Spec(rgb) => format!("{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b),
            }
        };
        let mut out = Vec::new();
        for index in lines.saturating_sub(limit)..lines {
            let row = &grid[self.grid_line(index)];
            let end = (0..columns)
                .rposition(|c| row[Column(c)].c != ' ' || row[Column(c)].bg != C::Named(NamedColor::Background));
            let mut style = String::new();
            for column in 0..end.map_or(0, |e| e + 1) {
                let cell = &row[Column(column)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut codes = vec!["0".to_owned(), sgr(cell.fg, true), sgr(cell.bg, false)];
                for (flag, code) in [
                    (Flags::BOLD, "1"),
                    (Flags::DIM, "2"),
                    (Flags::ITALIC, "3"),
                    (Flags::UNDERLINE, "4"),
                    (Flags::INVERSE, "7"),
                    (Flags::STRIKEOUT, "9"),
                ] {
                    if cell.flags.contains(flag) {
                        codes.push(code.to_owned());
                    }
                }
                let next = codes.join(";");
                if next != style {
                    out.extend_from_slice(format!("\x1b[{next}m").as_bytes());
                    style = next;
                }
                let c = if cell.c == '\t' { ' ' } else { cell.c };
                out.extend_from_slice(c.to_string().as_bytes());
                for &mark in cell.zerowidth().unwrap_or(&[]) {
                    out.extend_from_slice(mark.to_string().as_bytes());
                }
            }
            out.extend_from_slice(b"\x1b[0m");
            if index + 1 < lines {
                out.extend_from_slice(b"\r\n");
            }
        }
        out
    }

    /// Grid line for output line `index` (0 = oldest scrollback line).
    pub fn grid_line(&self, index: usize) -> Line {
        Line(index as i32 - self.term.grid().history_size() as i32)
    }

    /// Text between two (line, column) points of this block's output.
    pub fn text_between(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let start = Point::new(self.grid_line(start.0), Column(start.1));
        let end = Point::new(self.grid_line(end.0), Column(end.1.min(self.size.columns.saturating_sub(1))));
        let text = self.term.bounds_to_string(start, end);
        text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n")
    }

    /// The link under output cell (`line`, `column`): an OSC 8 hyperlink,
    /// or a URL in the text, followed across soft-wrapped lines.
    pub fn link_at(&self, line: usize, column: usize) -> Option<Link> {
        let grid = self.term.grid();
        let columns = grid.columns();
        let top = -(grid.history_size() as i32);
        let bottom = grid.screen_lines() as i32 - 1;
        let target = self.grid_line(line);
        if target.0 < top || target.0 > bottom || column >= columns {
            return None;
        }
        let output_line = |l: i32| (l - top) as usize;
        let mut first = target.0;
        let mut last = target.0;
        if let Some(link) = grid[target][Column(column)].hyperlink() {
            // Every cell carrying the same link, in reading order.
            let same = |l: i32, c: usize| grid[Line(l)][Column(c)].hyperlink().as_ref() == Some(&link);
            let (mut l0, mut c0) = (target.0, column);
            loop {
                let previous = if c0 > 0 { (l0, c0 - 1) } else { (l0 - 1, columns - 1) };
                if previous.0 < top || !same(previous.0, previous.1) {
                    break;
                }
                (l0, c0) = previous;
            }
            let (mut l1, mut c1) = (target.0, column);
            loop {
                let next = if c1 + 1 < columns { (l1, c1 + 1) } else { (l1 + 1, 0) };
                if next.0 > bottom || !same(next.0, next.1) {
                    break;
                }
                (l1, c1) = next;
            }
            let cells = (l0..=l1)
                .map(|l| {
                    let from = if l == l0 { c0 } else { 0 };
                    let to = if l == l1 { c1 } else { columns - 1 };
                    (output_line(l), from, to)
                })
                .collect();
            return Some(Link { url: link.uri().to_owned(), cells });
        }
        let wraps = |l: i32| grid[Line(l)][Column(columns - 1)].flags.contains(Flags::WRAPLINE);
        while first > top && wraps(first - 1) {
            first -= 1;
        }
        while last < bottom && wraps(last) {
            last += 1;
        }
        let mut chars = Vec::new();
        let mut at = Vec::new();
        for l in first..=last {
            for c in 0..columns {
                let cell = &grid[Line(l)][Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                chars.push(cell.c);
                at.push((l, c));
            }
        }
        let index = at.iter().position(|&(l, c)| l == target.0 && c == column)?;
        let url = find_urls(&chars).into_iter().find(|(start, end)| (*start..*end).contains(&index));
        let (start, end, url) = match url {
            Some((start, end)) => (start, end, chars[start..end].iter().collect()),
            None => {
                // A file or folder that exists, named absolutely, from ~,
                // or from where the command ran.
                let (start, end) = find_path(&chars, index)?;
                let name: String = chars[start..end].iter().collect();
                let path = if let Some(rest) = name.strip_prefix("~/") {
                    format!("{}/{rest}", std::env::var("HOME").ok()?)
                } else if name.starts_with('/') {
                    name
                } else {
                    format!("{}/{name}", self.cwd)
                };
                if !std::path::Path::new(&path).exists() {
                    return None;
                }
                (start, end, path)
            }
        };
        let mut cells: Vec<(usize, usize, usize)> = Vec::new();
        for &(l, c) in &at[start..end] {
            match cells.last_mut() {
                Some(run) if run.0 == output_line(l) => run.2 = c,
                _ => cells.push((output_line(l), c, c)),
            }
        }
        Some(Link { url, cells })
    }

    pub fn output_text(&mut self) -> String {
        let lines = self.output_lines();
        if lines == 0 {
            return String::new();
        }
        let text = self.text_between((0, 0), (lines - 1, self.size.columns));
        text.trim_end_matches('\n').to_owned()
    }

    pub(super) fn resize(&mut self, size: GridSize) {
        if self.size.columns != size.columns || self.size.lines != size.lines {
            let rewrap = self.size.columns != size.columns;
            self.term.resize(size);
            self.size = size;
            self.lines = None;
            self.generation += 1;
            if rewrap {
                self.lay_prose();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(kind: BlockKind, command: &str, columns: usize, lines: usize) -> Block {
        let listener = Listener(Rc::new(RefCell::new(Vec::new())));
        Block::new(1, kind, command.into(), "/".into(), None, GridSize { columns, lines }, listener)
    }

    fn finished(output: &[u8]) -> Block {
        let mut block = block(BlockKind::Command, "cmd", 20, 5);
        block.feed(output);
        block.finished = Some(Instant::now());
        block.exit = Some(0);
        block
    }

    #[test]
    fn clear_commands_are_recognised_and_nothing_else() {
        for command in ["clear", "  reset ", "cls"] {
            assert!(block(BlockKind::Command, command, 20, 5).clears, "{command}");
        }
        for command in ["clear-cache", "echo clear", "clear; ls", ""] {
            assert!(!block(BlockKind::Command, command, 20, 5).clears, "{command}");
        }
        assert!(!block(BlockKind::Ai, "clear", 20, 5).clears);
        assert!(!block(BlockKind::Background, "clear", 20, 5).clears);
    }

    #[test]
    fn only_a_nonzero_exit_is_a_failure() {
        let mut block = block(BlockKind::Command, "false", 20, 5);
        assert!(block.running() && !block.failed());
        block.exit = Some(0);
        assert!(!block.failed());
        block.exit = Some(1);
        assert!(block.failed());
        block.exit = Some(-1);
        assert!(block.failed());
        let at = block.started + Duration::from_millis(1500);
        block.finished = Some(at);
        assert!(!block.running());
        assert_eq!(block.duration(), Duration::from_millis(1500));
    }

    #[test]
    fn output_ends_at_the_last_non_blank_line() {
        let mut block = finished(b"one\r\ntwo\r\n\r\n   \r\n");
        assert_eq!(block.output_lines(), 2);
        assert_eq!(block.output_text(), "one\ntwo");
        let mut empty = finished(b"");
        assert_eq!(empty.output_lines(), 0);
        assert_eq!(empty.output_text(), "");
    }

    #[test]
    fn a_running_block_shows_down_to_the_cursor() {
        let mut block = block(BlockKind::Command, "cmd", 20, 5);
        assert_eq!(block.output_lines(), 0);
        block.feed(b"one\r\n\r\n");
        // The cursor sits two lines down, where the next output goes.
        assert_eq!(block.output_lines(), 3);
        block.feed(b"x");
        assert_eq!(block.output_lines(), 3);
    }

    #[test]
    fn scrolled_off_lines_still_count_as_output() {
        let mut block = finished(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8");
        assert_eq!(block.output_lines(), 8);
        assert_eq!(block.output_text(), "1\n2\n3\n4\n5\n6\n7\n8");
        assert_eq!(block.text_between((0, 0), (1, 20)), "1\n2");
        assert_eq!(block.text_between((7, 0), (7, 0)), "8");
    }

    #[test]
    fn text_keeps_wide_characters_and_emoji_whole() {
        let mut block = finished("中文 ok\r\n👍🏽 done".as_bytes());
        assert_eq!(block.output_text(), "中文 ok\n👍🏽 done");
    }

    #[test]
    fn output_ansi_brings_back_colors_and_styles() {
        use alacritty_terminal::vte::ansi::{Color as C, NamedColor, Rgb};
        let mut original =
            finished(b"\x1b[1;31mbold red\x1b[0m \x1b[38;5;208mo\x1b[48;2;1;2;3mx\x1b[0m\r\n\x1b[4mu\x1b[0m");
        let ansi = original.output_ansi(100);
        let mut copy = finished(&ansi);
        assert_eq!(copy.output_text(), original.output_text());
        let grid = copy.term.grid();
        let line0 = &grid[copy.grid_line(0)];
        assert_eq!(line0[Column(0)].fg, C::Named(NamedColor::Red));
        assert!(line0[Column(0)].flags.contains(Flags::BOLD));
        assert_eq!(line0[Column(9)].fg, C::Indexed(208));
        assert_eq!(line0[Column(10)].bg, C::Spec(Rgb { r: 1, g: 2, b: 3 }));
        assert!(grid[copy.grid_line(1)][Column(0)].flags.contains(Flags::UNDERLINE));
        // Only the last `limit` lines.
        let mut tail = finished(&original.output_ansi(1));
        assert_eq!(tail.output_text(), "u");
    }

    #[test]
    fn resizing_to_the_same_size_changes_nothing() {
        let mut block = finished(b"hello");
        let generation = block.generation;
        block.resize(GridSize { columns: 20, lines: 5 });
        assert_eq!(block.generation, generation);
        block.resize(GridSize { columns: 10, lines: 5 });
        assert_eq!(block.generation, generation + 1);
        assert_eq!(block.output_text(), "hello");
    }

    #[test]
    fn every_feed_counts_as_a_change() {
        let mut block = block(BlockKind::Command, "cmd", 20, 5);
        let start = block.generation;
        block.feed(b"a");
        block.feed(b"");
        assert_eq!(block.generation, start + 2);
    }

    #[test]
    fn the_alternate_screen_is_noticed() {
        let mut block = block(BlockKind::Command, "vim", 20, 5);
        assert!(!block.alt_screen());
        block.feed(b"\x1b[?1049h");
        assert!(block.alt_screen());
        block.feed(b"\x1b[?1049l");
        assert!(!block.alt_screen());
    }

    #[test]
    fn links_outside_the_output_are_none() {
        let block = finished(b"https://x.io");
        assert_eq!(block.link_at(0, 3).map(|l| l.url).as_deref(), Some("https://x.io"));
        assert_eq!(block.link_at(0, 12), None);
        assert_eq!(block.link_at(0, 20), None);
        assert_eq!(block.link_at(99, 0), None);
    }

    #[test]
    fn links_skip_the_spacers_of_wide_characters() {
        let block = finished("中文 https://x.io".as_bytes());
        // 中文 take columns 0-3; the URL starts at column 5.
        let link = block.link_at(0, 7).expect("a link after wide characters");
        assert_eq!(link.url, "https://x.io");
        assert_eq!(link.cells, [(0, 5, 16)]);
    }
}

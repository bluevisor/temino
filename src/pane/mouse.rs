use super::*;

impl Pane {
    pub(super) fn select_neighbor(&mut self, direction: i32) {
        let visible: Vec<u64> = self.session.blocks.iter().map(|b| b.id).collect();
        if visible.is_empty() {
            return;
        }
        let index = match self.selected.and_then(|id| visible.iter().position(|&v| v == id)) {
            Some(i) => (i as i32 + direction).clamp(0, visible.len() as i32 - 1) as usize,
            None if direction < 0 => visible.len() - 1,
            None => return,
        };
        self.selected = Some(visible[index]);
        self.scroll_into_view(visible[index]);
    }
    pub(super) fn scroll_into_view(&mut self, id: u64) {
        let Some(layout) = self.layout.blocks.iter().find(|b| b.id == id) else {
            // Off screen: lay out happens in render; approximate by index.
            return;
        };
        let list = self.layout.list;
        if layout.rect.y < list.y {
            self.scroll_by(list.y - layout.rect.y + 8.0);
        } else if layout.rect.bottom() > list.bottom() {
            self.scroll_by(-(layout.rect.bottom() - list.bottom()).min(layout.rect.y - list.y));
        }
    }
    /// The text under the mouse selection, if any.
    pub(super) fn selection_text(&self) -> Option<String> {
        let selection = self.selection?;
        let block = self.session.blocks.iter().find(|b| b.id == selection.block)?;
        let mut text = String::new();
        if let Some(((l0, c0), (l1, c1))) = self.command_span(block.id) {
            let rows = command_rows(&block.command, self.session.size.columns.max(1));
            for (index, (row, ends_line)) in rows.iter().enumerate().take(l1 + 1).skip(l0) {
                let chars: Vec<char> = row.chars().collect();
                let from = if index == l0 { c0 } else { 0 };
                let to = if index == l1 { c1 } else { usize::MAX };
                let (from, to) = row_char_range(row, from, to);
                text.extend(&chars[from..to]);
                if *ends_line && index < l1 {
                    text.push('\n');
                }
            }
        }
        if let Some((start, end)) = self.selection_span(block.id) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&block.text_between(start, end));
        }
        Some(text).filter(|text| !text.is_empty())
    }
    pub(super) fn copy(&mut self, ctx: &mut Ctx, command_only: bool) {
        let text = if let Some(text) = self.editor.selected_text().filter(|_| self.session.editor_active()) {
            Some(text.to_owned())
        } else if self.selection.is_some() {
            self.selection_text()
        } else if let Some(id) = self.selected {
            self.session.blocks.iter_mut().find(|b| b.id == id).map(|block| {
                if command_only {
                    block.command.clone()
                } else {
                    block.output_text()
                }
            })
        } else {
            None
        };
        match text {
            Some(text) if !text.is_empty() => {
                platform::clipboard_set(&text);
                let lines = text.lines().count();
                ctx.show_toast(&if lines > 1 { format!("Copied {lines} lines") } else { "Copied".to_owned() });
            }
            _ => platform::beep(),
        }
    }
    /// The pane menu's Copy, Paste and Clear.
    pub fn copy_selection(&mut self, ctx: &mut Ctx) {
        self.copy(ctx, false);
    }
    pub fn paste_clipboard(&mut self) {
        self.paste(platform::clipboard_get());
    }
    pub fn clear(&mut self) {
        self.clear_blocks();
    }
    /// Whether the full-screen program here takes mouse clicks itself.
    pub fn reports_mouse(&self) -> bool {
        self.layout.alt.is_some_and(|(index, _, _)| {
            self.session.blocks.get(index).is_some_and(|block| block.term.mode().intersects(TermMode::MOUSE_MODE))
        })
    }
    /// Text an input method committed: typed, not pasted.
    pub fn text_input(&mut self, text: &str) {
        self.recap = None;
        if self.session.active().is_some() {
            self.track_typing(text);
            self.session.write(text.as_bytes());
            self.selection = None;
            self.pin_to_bottom();
        } else if self.session.editor_active() {
            self.editor.insert(text);
            self.selected = None;
        }
    }
    /// Bytes for the running program, as they are (a key binding's `\xff`
    /// is one byte, not UTF-8).
    pub fn type_bytes(&mut self, bytes: &[u8]) {
        self.recap = None;
        self.track_typing(&String::from_utf8_lossy(bytes));
        self.session.write(bytes);
        self.selection = None;
        self.pin_to_bottom();
    }
    pub(super) fn paste(&mut self, text: Option<String>) {
        if let Some(block) = self.session.active() {
            let bracketed = block.term.mode().contains(TermMode::BRACKETED_PASTE);
            if bracketed && text.as_deref().is_none_or(str::is_empty) {
                // No text: likely an image. An empty paste still tells the
                // program to look, and Claude Code then reads the image
                // from the clipboard itself.
                self.session.write(b"\x1b[200~\x1b[201~");
                return;
            }
        }
        let Some(text) = text else { return };
        let text = text.replace("\r\n", "\n");
        if let Some(block) = self.session.active() {
            let bracketed = block.term.mode().contains(TermMode::BRACKETED_PASTE);
            let body = text.replace('\n', "\r");
            self.track_typing(&text.replace('\n', " "));
            if bracketed {
                self.session.write(format!("\x1b[200~{}\x1b[201~", body.replace("\x1b[201~", "")).as_bytes());
            } else {
                self.session.write(body.as_bytes());
            }
        } else if self.session.editor_active() {
            self.editor.insert(&text);
        }
    }
    pub fn wheel(&mut self, ctx: &Ctx, dy: f32, precise: bool, x: f32, y: f32) {
        if let Some(block) = self.session.active() {
            if block.alt_screen() {
                let mode = *block.term.mode();
                // A trackpad steps the program once a line of travel (a
                // notch is three), so it starts moving at once; a wheel
                // steps once a notch. Whole steps only: trackpads send
                // fractions, and a turn back drops what was left over.
                let steps = if precise { dy * 3.0 } else { dy };
                if steps * self.wheel_rest < 0.0 {
                    self.wheel_rest = 0.0;
                }
                self.wheel_rest += steps;
                let lines = self.wheel_rest.trunc();
                self.wheel_rest -= lines;
                if lines == 0.0 {
                    return;
                }
                if mode.intersects(TermMode::MOUSE_MODE) {
                    if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                        let button = if lines < 0.0 { 64 } else { 65 };
                        for _ in 0..lines.abs() as usize {
                            self.mouse_report(button, column, line, true);
                        }
                    }
                } else if mode.contains(TermMode::ALTERNATE_SCROLL) {
                    let arrow: &[u8] = if mode.contains(TermMode::APP_CURSOR) {
                        if lines < 0.0 {
                            b"\x1bOA"
                        } else {
                            b"\x1bOB"
                        }
                    } else if lines < 0.0 {
                        b"\x1b[A"
                    } else {
                        b"\x1b[B"
                    };
                    let per_step = if precise { 1 } else { 3 };
                    self.session.write(&arrow.repeat(lines.abs() as usize * per_step));
                }
                return;
            }
        }
        self.scroll_by(-dy * 3.0 * ctx.text.cell.height);
        // A trackpad's deltas are already smooth: follow the fingers
        // exactly instead of easing after them.
        if precise {
            self.scroll = self.scroll_target;
        }
    }
    pub(super) fn alt_cell(&self, ctx: &Ctx, x: f32, y: f32) -> Option<(usize, usize)> {
        let (_, ox, oy) = self.layout.alt?;
        let cell = ctx.text.cell;
        let column = ((x - ox) / cell.width).floor().max(0.0) as usize;
        let line = ((y - oy) / cell.height).floor().max(0.0) as usize;
        Some((column.min(self.session.size.columns - 1), line.min(self.session.size.lines - 1)))
    }
    pub(super) fn mouse_report(&self, button: u8, column: usize, line: usize, pressed: bool) {
        let Some(block) = self.session.active() else { return };
        let mode = *block.term.mode();
        if mode.contains(TermMode::SGR_MOUSE) {
            let end = if pressed { 'M' } else { 'm' };
            self.session.write(format!("\x1b[<{button};{};{}{end}", column + 1, line + 1).as_bytes());
        } else if column < 223 && line < 223 {
            let code = if pressed { button } else { 3 };
            self.session.write(&[0x1b, b'[', b'M', 32 + code, 33 + column as u8, 33 + line as u8]);
        }
    }
    pub fn mouse_down(&mut self, ctx: &Ctx, x: f32, y: f32, pressed: i32, clicks: i32, mods: u32) {
        let shift = mods & platform::mods::SHIFT != 0;
        let button = if pressed & buttons::SECONDARY != 0 {
            2
        } else if pressed & buttons::TERTIARY != 0 {
            1
        } else {
            0
        };
        self.link_click = None;
        if let Some((track, thumb)) = self.layout.scrollbar.filter(|(track, _)| button == 0 && track.contains(x, y)) {
            // Grab the thumb where it was pressed; a press beside it jumps
            // there and grabs it by the middle.
            let top = self.thumb_top(track, thumb);
            let grab = if (top..top + thumb).contains(&y) { y - top } else { thumb / 2.0 };
            self.bar_grab = Some(grab);
            self.drag_scrollbar(y);
            return;
        }
        if button == 0 && clicks == 1 {
            if let Some((_, link)) = self.link_under(ctx, x, y) {
                // Cmd+click opens in the system's app at once, even over
                // programs that take the mouse; a plain click waits to see
                // it isn't a drag, then the app opens it beside the pane.
                if mods & platform::mods::COMMAND != 0 {
                    platform::open_url(&link.url);
                    return;
                }
                self.link_click = Some(link.url);
            }
        }
        if let Some((index, _, _)) = self.layout.alt {
            let block = &self.session.blocks[index];
            let mode = *block.term.mode();
            if mode.intersects(TermMode::MOUSE_MODE) && !shift {
                if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                    self.mouse_report(button, column, line, true);
                }
                return;
            }
            if button == 1 {
                return self.paste(platform::selection_get());
            }
            if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                let id = block.id;
                self.start_selection(id, false, line, column, clicks);
            }
            return;
        }
        // Middle click pastes the primary selection, where there is one.
        if button == 1 {
            return self.paste(platform::selection_get());
        }
        if let Some((_, item)) = self.layout.menu_rows.iter().find(|(rect, _)| rect.contains(x, y)) {
            let item = *item;
            if let Some(completion) = &mut self.completion {
                completion.selected = item;
            }
            self.accept_completion();
            return;
        }
        self.completion = None;
        if self.layout.input.contains(x, y) {
            // A click places the caret and starts a drag selection, a double
            // click selects a word and a triple click the line, like any text
            // field; Shift+click extends the selection.
            self.selected = None;
            if !self.session.editor_active() {
                return;
            }
            if shift || button != 0 {
                self.place_editor_cursor(ctx, x, y, shift);
                return;
            }
            match clicks {
                n if n >= 2 => {
                    // the character under the pointer, not the nearest gap
                    let Some(at) = self.editor_offset_at(ctx, x, y, false) else { return };
                    self.editor.cursor = at;
                    if clicks == 2 {
                        self.editor.select_word();
                    } else {
                        self.editor.select_line();
                    }
                }
                _ => {
                    self.place_editor_cursor(ctx, x, y, false);
                    self.editor.anchor = Some(self.editor.cursor);
                    self.editor_dragging = true;
                }
            }
            return;
        }
        let hit = self
            .layout
            .blocks
            .iter()
            .find(|b| b.rect.contains(x, y))
            .map(|b| (b.id, b.output_x, b.output_y, b.lines, b.command_y, b.command_rows));
        let Some((id, ox, oy, lines, cy, rows)) = hit else {
            self.selected = None;
            self.selection = None;
            return;
        };
        let cell = ctx.text.cell;
        let columns = self.session.size.columns;
        let column = (((x - ox) / cell.width).max(0.0) as usize).min(columns - 1);
        if button == 0 && lines > 0 && y >= oy && y < oy + lines as f32 * cell.height {
            let line = ((y - oy) / cell.height) as usize;
            self.start_selection(id, false, line.min(lines - 1), column, clicks);
            self.selected = Some(id);
        } else if button == 0 && rows > 0 && y >= cy && y < cy + rows as f32 * cell.height {
            let row = ((y - cy) / cell.height) as usize;
            self.start_selection(id, true, row.min(rows - 1), column, clicks);
            self.selected = Some(id);
        } else {
            self.selection = None;
            self.selected = if self.selected == Some(id) && button == 0 { None } else { Some(id) };
        }
    }
    pub(super) fn start_selection(&mut self, block: u64, command: bool, line: usize, column: usize, clicks: i32) {
        let mode = match clicks {
            2 => SelectMode::Word,
            n if n >= 3 => SelectMode::Line,
            _ => SelectMode::Char,
        };
        let spot = (!command, line, column);
        self.selection = Some(Selection { block, anchor: spot, head: spot, mode });
        self.dragging = true;
    }
    /// The pointer moved over (or, while dragging, away from) the pane;
    /// returns the pointer shape it wants.
    pub fn mouse_moved(&mut self, ctx: &Ctx, x: f32, y: f32, buttons: i32) -> Cursor {
        // Pointer shape: an arrow over chrome, the I-beam over text.
        let over_text = self.layout.alt.is_some()
            || self.layout.input.contains(x, y)
            || self.layout.blocks.iter().any(|b| b.rect.contains(x, y) && y >= b.command_y);
        self.pointer = Some((x, y));
        if self.bar_grab.is_some() && buttons != 0 {
            self.drag_scrollbar(y);
            return Cursor::Arrow;
        }
        if self.layout.scrollbar.is_some_and(|(track, _)| track.contains(x, y)) {
            return Cursor::Arrow;
        }
        let cursor = if buttons == 0 && self.link_under(ctx, x, y).is_some() {
            Cursor::Link
        } else if over_text {
            Cursor::IBeam
        } else {
            Cursor::Arrow
        };

        if let Some((index, _, _)) = self.layout.alt {
            let mode = *self.session.blocks[index].term.mode();
            if buttons != 0 && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                    self.mouse_report(32, column, line, true);
                }
                return cursor;
            }
        }
        if self.editor_dragging {
            if buttons != 0 {
                if let Some(at) = self.editor_offset_at(ctx, x, y, true) {
                    self.editor.cursor = at;
                }
            }
            return cursor;
        }
        if !self.dragging || buttons == 0 {
            return cursor;
        }
        let Some(mut selection) = self.selection else { return cursor };
        let cell = ctx.text.cell;
        let spot = if let Some((_, ox, oy)) = self.layout.alt {
            let column = ((x - ox) / cell.width).max(0.0) as usize;
            let line = ((y - oy) / cell.height).max(0.0) as usize;
            (true, line.min(self.session.size.lines - 1), column)
        } else {
            let Some(layout) = self.layout.blocks.iter().find(|b| b.id == selection.block) else { return cursor };
            // Above the output, the command's rows; below them, the output's
            // lines: one drag can take both.
            let in_command = layout.command_rows > 0 && (layout.lines == 0 || y < layout.output_y);
            let (top, lines) =
                if in_command { (layout.command_y, layout.command_rows) } else { (layout.output_y, layout.lines) };
            if lines == 0 {
                return cursor;
            }
            let line = ((y - top) / cell.height).floor().clamp(0.0, (lines - 1) as f32) as usize;
            let column = ((x - layout.output_x) / cell.width).max(0.0) as usize;
            (!in_command, line, column)
        };
        let (output, line, column) = spot;
        selection.head = (output, line, column.min(self.session.size.columns - 1));
        self.selection = Some(selection);
        cursor
    }
    pub fn mouse_up(&mut self, ctx: &Ctx, x: f32, y: f32) {
        self.editor_dragging = false;
        let link = self.link_click.take();
        if let Some((index, _, _)) = self.layout.alt {
            let mode = *self.session.blocks[index].term.mode();
            if mode.intersects(TermMode::MOUSE_MODE) {
                if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                    self.mouse_report(0, column, line, false);
                }
                // The program takes the mouse, so there's no selection to
                // tell a click from a drag: a release on the link it was
                // pressed on opens it.
                self.clicked_link =
                    link.filter(|url| self.link_under(ctx, x, y).is_some_and(|(_, under)| under.url == *url));
                self.bar_grab = None;
                self.dragging = false;
                return;
            }
        }
        self.bar_grab = None;
        let was_dragging = std::mem::take(&mut self.dragging);
        if let Some(selection) = self.selection {
            if selection.mode == SelectMode::Char && selection.anchor == selection.head {
                self.selection = None;
                self.clicked_link = link;
            } else if was_dragging {
                if let Some(text) = self.selection_text() {
                    platform::selection_set(&text);
                }
            }
        }
    }
    /// Where the scroll bar's thumb starts, for the current scroll.
    pub(super) fn thumb_top(&self, track: Rect, thumb: f32) -> f32 {
        track.y + (1.0 - self.scroll / self.max_scroll().max(1.0)) * (track.h - thumb)
    }
    /// Scrolls so the grabbed point of the thumb sits at `y`.
    pub(super) fn drag_scrollbar(&mut self, y: f32) {
        let (Some((track, thumb)), Some(grab)) = (self.layout.scrollbar, self.bar_grab) else { return };
        let travel = (track.h - thumb).max(1.0);
        let fraction = ((y - grab - track.y) / travel).clamp(0.0, 1.0);
        self.scroll_target = (1.0 - fraction) * self.max_scroll();
        self.scroll = self.scroll_target;
    }
    /// The pointer left this pane.
    pub fn pointer_left(&mut self) {
        self.pointer = None;
    }
    /// The link under pane point (`x`, `y`), with the index of its block.
    pub(super) fn link_under(&self, ctx: &Ctx, x: f32, y: f32) -> Option<(usize, Link)> {
        let cell = ctx.text.cell;
        let (index, ox, oy, lines) = if let Some((index, ox, oy)) = self.layout.alt {
            (index, ox, oy, self.session.size.lines)
        } else {
            let hit = self.layout.blocks.iter().find(|b| b.rect.contains(x, y))?;
            let index = self.session.blocks.iter().position(|b| b.id == hit.id)?;
            (index, hit.output_x, hit.output_y, hit.lines)
        };
        if x < ox || y < oy || self.layout.input.contains(x, y) {
            return None;
        }
        if self.layout.scrollbar.is_some_and(|(track, _)| track.contains(x, y)) {
            return None;
        }
        let column = ((x - ox) / cell.width) as usize;
        let line = ((y - oy) / cell.height) as usize;
        if column >= self.session.size.columns || line >= lines {
            return None;
        }
        let block = &self.session.blocks[index];
        // The alternate screen is drawn from its top, not its scrollback.
        let line = if self.layout.alt.is_some() { line + block.term.grid().history_size() } else { line };
        block.link_at(line, column).map(|link| (index, link))
    }
    /// Underlines the link under the pointer.
    pub(super) fn draw_link(&self, scene: &mut Scene, ctx: &Ctx) {
        let Some((x, y)) = self.pointer else { return };
        let Some((index, link)) = self.link_under(ctx, x, y) else { return };
        let cell = ctx.text.cell;
        let block = &self.session.blocks[index];
        let (ox, oy, skip) = if let Some((_, ox, oy)) = self.layout.alt {
            (ox, oy, block.term.grid().history_size())
        } else {
            let Some(layout) = self.layout.blocks.iter().find(|b| b.id == block.id) else { return };
            (layout.output_x, layout.output_y, 0)
        };
        let input_top = if self.layout.input.h > 0.0 { self.layout.input.y } else { f32::MAX };
        for &(line, from, to) in &link.cells {
            let Some(line) = line.checked_sub(skip) else { continue };
            let row = oy + line as f32 * cell.height;
            if row + cell.height > input_top {
                continue;
            }
            let rect = Rect::new(
                ox + from as f32 * cell.width,
                row + cell.height - 2.0,
                (to + 1 - from) as f32 * cell.width,
                1.0,
            );
            scene.fill(rect, ctx.theme.accent);
        }
    }
    /// The command input's byte offset at (`x`, `y`): the nearest gap
    /// between characters, or with `gap` false the character under it.
    pub(super) fn editor_offset_at(&self, ctx: &Ctx, x: f32, y: f32, gap: bool) -> Option<usize> {
        if !self.session.editor_active() {
            return None;
        }
        let cell = ctx.text.cell;
        let line = ((y - self.layout.editor_y) / cell.height).floor().max(0.0) as usize;
        let column = (x - self.layout.editor_x) / cell.width;
        let column = if gap { column.round() } else { column.floor() }.max(0.0) as usize;
        let line = line.min(self.editor.line_count() - 1);
        Some(self.editor.offset(line, column))
    }
    pub(super) fn place_editor_cursor(&mut self, ctx: &Ctx, x: f32, y: f32, extend: bool) {
        let Some(to) = self.editor_offset_at(ctx, x, y, true) else { return };
        if extend {
            self.editor.anchor.get_or_insert(self.editor.cursor);
        } else {
            self.editor.anchor = None;
        }
        self.editor.cursor = to;
    }
}

pub(super) fn normalize(selection: Selection) -> (Spot, Spot) {
    if selection.anchor <= selection.head {
        (selection.anchor, selection.head)
    } else {
        (selection.head, selection.anchor)
    }
}

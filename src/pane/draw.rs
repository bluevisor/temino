use super::*;

impl Pane {
    /// The top of the command input, in pane coordinates (toasts sit above it).
    pub fn input_top(&self) -> f32 {
        if self.layout.input.h > 0.0 {
            self.layout.input.y
        } else {
            self.rect.h
        }
    }
    /// Draws the pane at its place in the window.
    pub fn draw(&mut self, scene: &mut Scene, ctx: &mut Ctx, focused: bool) {
        scene.set_origin(self.rect.x, self.rect.y);
        scene.push_clip(Rect::new(0.0, 0.0, self.rect.w, self.rect.h));
        self.layout = Layout::default();
        let alt = self.session.active().is_some_and(|b| b.alt_screen());
        if alt {
            self.draw_alt(scene, ctx, focused);
        } else {
            self.draw_main(scene, ctx, focused);
        }
        self.draw_link(scene, ctx);
        self.draw_find(scene, ctx);
        if self.bell.is_some() {
            scene.set_layer(LAYER_OVERLAY);
            let edge = Rect::new(0.0, 0.0, self.rect.w, self.rect.h);
            scene.quad(Quad::new(edge, Color::TRANSPARENT).border(3.0, ctx.theme.warning.alpha(0.7)));
            scene.set_layer(LAYER_CONTENT);
        }
        scene.pop_clip();
        scene.set_origin(0.0, 0.0);
    }
    pub(super) fn selection_span(&self, id: u64) -> Option<Span> {
        let selection = self.selection.filter(|s| s.block == id)?;
        let block = self.session.blocks.iter().find(|b| b.id == id)?;
        let (from, to) = normalize(selection);
        if !to.0 {
            return None;
        }
        // From the command: the output from its start.
        let mut start = if from.0 { (from.1, from.2) } else { (0, 0) };
        let mut end = (to.1, to.2);
        let columns = self.session.size.columns;
        match selection.mode {
            SelectMode::Char => {}
            SelectMode::Line => {
                start.1 = 0;
                end.1 = columns - 1;
            }
            SelectMode::Word => {
                let grid = block.term.grid();
                let is_word = |line: usize, column: usize| {
                    let c = grid[block.grid_line(line)][alacritty_terminal::index::Column(column)].c;
                    !c.is_whitespace() && !"()[]{}<>'\"`|;,".contains(c)
                };
                while start.1 > 0 && is_word(start.0, start.1 - 1) {
                    start.1 -= 1;
                }
                while end.1 + 1 < columns && is_word(end.0, end.1 + 1) {
                    end.1 += 1;
                }
            }
        }
        Some((start, end))
    }
    /// The selection within a block's command, in its wrapped rows.
    pub(super) fn command_span(&self, id: u64) -> Option<Span> {
        let selection = self.selection.filter(|s| s.block == id)?;
        let block = self.session.blocks.iter().find(|b| b.id == id)?;
        let (from, to) = normalize(selection);
        if from.0 {
            return None;
        }
        let columns = self.session.size.columns.max(1);
        let mut start = (from.1, from.2);
        // Into the output: the command to its end.
        let mut end = if to.0 {
            (command_rows(&block.command, columns).len().saturating_sub(1), columns - 1)
        } else {
            (to.1, to.2)
        };
        match selection.mode {
            SelectMode::Char => {}
            SelectMode::Line => {
                start.1 = 0;
                end.1 = columns - 1;
            }
            SelectMode::Word => {
                let rows = command_rows(&block.command, columns);
                let row = |line: usize| rows.get(line).map(|(s, _)| s.as_str()).unwrap_or("");
                let is_word = |c: char| !c.is_whitespace() && !"()[]{}<>'\"`|;,".contains(c);
                // The span's columns are cells; expand over whole words by
                // going through the rows' characters and back.
                let (first, last) = (row(start.0), row(end.0));
                let first_chars: Vec<char> = first.chars().collect();
                let (mut at, _) = row_char_range(first, start.1, start.1);
                while at > 0 && first_chars.get(at - 1).is_some_and(|&c| is_word(c)) {
                    at -= 1;
                }
                start.1 = row_cell_of_char(first, at);
                let last_chars: Vec<char> = last.chars().collect();
                let (_, mut at) = row_char_range(last, end.1, end.1);
                while last_chars.get(at).is_some_and(|&c| is_word(c)) {
                    at += 1;
                }
                if at > 0 {
                    end.1 = row_cell_of_char(last, at - 1) + crate::editor::width(last_chars[at - 1]) - 1;
                }
            }
        }
        Some((start, end))
    }
    pub(super) fn draw_alt(&mut self, scene: &mut Scene, ctx: &mut Ctx, focused: bool) {
        let Some(index) = self.session.blocks.iter().rposition(|b| b.running() && b.kind == BlockKind::Command) else {
            return;
        };
        let id = self.session.blocks[index].id;
        let selection = self.selection_span(id);
        let blink = ctx.blink_on();
        let (x, y) = (PAD_X, ALT_PAD_Y);
        self.layout.alt = Some((index, x, y));
        let lines = self.session.blocks[index].term.grid().screen_lines();
        let view = GridView {
            block: &self.session.blocks[index],
            x,
            y,
            first: 0,
            last: lines,
            cursor: true,
            focused,
            selection,
            marks: Vec::new(),
            blink_on: blink,
        };
        ui::draw_grid(scene, &mut ctx.text, &ctx.theme, view);
    }
    pub(super) fn block_height(&mut self, text: &TextSystem, index: usize) -> (f32, usize) {
        let cell = text.cell;
        let columns = self.session.size.columns.max(1);
        let block = &mut self.session.blocks[index];
        let command_lines =
            if block.kind != BlockKind::Background { command_rows(&block.command, columns).len() } else { 0 };
        let output = if block.alt_screen() { 0 } else { block.output_lines() };
        let mut height = BLOCK_PAD_TOP + META_HEIGHT + command_lines as f32 * cell.height + BLOCK_PAD_BOTTOM;
        if output > 0 {
            height += OUTPUT_GAP + output as f32 * cell.height;
        } else if waiting_on_ai(block, output) {
            // Room for the "Thinking…" line.
            height += OUTPUT_GAP + cell.height;
        }
        (height + 1.0, output)
    }
    pub(super) fn input_height(&self, text: &TextSystem) -> f32 {
        let lines = if self.session.editor_active() { self.editor.line_count().min(MAX_EDITOR_LINES) } else { 1 };
        INPUT_PAD + CHIP_HEIGHT + 8.0 + lines as f32 * text.cell.height + INPUT_PAD + 2.0
    }
    pub(super) fn draw_main(&mut self, scene: &mut Scene, ctx: &mut Ctx, focused: bool) {
        let (width, height) = (self.rect.w, self.rect.h);
        let input_height = self.input_height(&ctx.text);
        let welcome = self.welcome && self.session.blocks.is_empty();
        let welcome_height = if welcome { self.welcome_height(&ctx.text) } else { 0.0 };

        let mut heights = Vec::with_capacity(self.session.blocks.len());
        let mut total = welcome_height;
        for index in 0..self.session.blocks.len() {
            let h = self.block_height(&ctx.text, index);
            total += h.0;
            heights.push(h);
        }
        // Keep the view still when output grows while scrolled up.
        let list_max = (height - input_height).max(0.0);
        let mut reveal = None;
        if let Some(find) = self.find.as_mut() {
            find.refresh(&self.session.blocks);
            if std::mem::take(&mut find.reveal) {
                reveal = find.current;
            }
        }
        if self.scroll_target > 0.0 && self.content_height > 0.0 && total > self.content_height {
            let grown = total - self.content_height;
            self.scroll += grown;
            self.scroll_target += grown;
        }
        self.content_height = total;
        let max_scroll = (total - list_max).max(0.0);
        if let Some(found) = reveal {
            // The match's line, a third of the way down the view.
            let mut y = welcome_height;
            for (block, (h, _)) in self.session.blocks.iter().zip(&heights) {
                if block.id == found.block {
                    let rows = if block.kind == BlockKind::Background {
                        0
                    } else {
                        command_rows(&block.command, self.session.size.columns.max(1)).len()
                    };
                    let cell = ctx.text.cell.height;
                    y += BLOCK_PAD_TOP + META_HEIGHT + rows as f32 * cell + OUTPUT_GAP + found.line as f32 * cell;
                    let scroll = (total - y - list_max * 2.0 / 3.0).clamp(0.0, max_scroll);
                    self.scroll = scroll;
                    self.scroll_target = scroll;
                    break;
                }
                y += h;
            }
        }
        self.scroll_target = self.scroll_target.min(max_scroll);
        self.scroll = self.scroll.min(max_scroll);
        let (top, input_y) =
            if total <= list_max { (0.0, total) } else { (list_max - total + self.scroll.round(), list_max) };
        self.layout.list = Rect::new(0.0, 0.0, width, input_y);
        self.layout.content_height = total;

        scene.push_clip(Rect::new(0.0, 0.0, width, input_y));
        if welcome_height > 0.0 {
            self.draw_welcome(scene, ctx, top);
        }
        let mut y = top + welcome_height;
        for (index, (block_height, output)) in heights.into_iter().enumerate() {
            if y + block_height > 0.0 && y < input_y {
                self.draw_block(scene, ctx, focused, index, y, block_height, output);
            }
            y += block_height;
        }
        scene.pop_clip();

        if total > list_max && list_max > 40.0 {
            let thumb = (list_max / total * list_max).max(24.0);
            let position = (1.0 - self.scroll / max_scroll.max(1.0)) * (list_max - thumb);
            let track = Rect::new(width - SCROLLBAR_HIT, 0.0, SCROLLBAR_HIT, list_max);
            self.layout.scrollbar = Some((track, thumb));
            // Wider and easier to grab under the pointer.
            let hovered = self.bar_grab.is_some() || self.pointer.is_some_and(|(x, y)| track.contains(x, y));
            if hovered {
                scene.fill(track, ctx.theme.text_faint.alpha(0.06));
                let bar = Rect::new(width - 10.0, position + 2.0, 7.0, thumb - 4.0);
                let alpha = if self.bar_grab.is_some() { 0.85 } else { 0.65 };
                scene.quad(Quad::new(bar, ctx.theme.text_faint.alpha(alpha)).radius(3.5));
            } else {
                let bar = Rect::new(width - 6.0, position + 2.0, 3.0, thumb - 4.0);
                scene.quad(Quad::new(bar, ctx.theme.text_faint.alpha(0.45)).radius(1.5));
            }
        }
        self.draw_input(scene, ctx, focused, input_y, input_height);
    }
    /// The ASCII-art banner for this pane's width, if any fits.
    pub(super) fn banner(&self) -> Option<&'static [&'static str]> {
        let columns = self.session.size.columns;
        if columns >= BANNER_LARGE_COLUMNS {
            Some(BANNER_LARGE)
        } else if columns >= BANNER[0].chars().count() + 2 {
            Some(BANNER)
        } else {
            None
        }
    }
    pub(super) fn welcome_height(&self, text: &TextSystem) -> f32 {
        let title = self.banner().map_or(30.0, |b| b.len() as f32 * text.cell.height);
        24.0 + title + 30.0 + 3.0 * 28.0 + 12.0
    }
    pub(super) fn draw_welcome(&mut self, scene: &mut Scene, ctx: &mut Ctx, top: f32) {
        let banner = self.banner();
        let Ctx { text, theme, device_name, .. } = ctx;
        let x = PAD_X + 6.0;
        let mut y = top + 24.0;
        if let Some(banner) = banner {
            // The three leaves of the Haiku logo, left to right: green,
            // orange, peach, blended by hue so green turns yellow on its way
            // to orange. The big banner's top rows catch a little light like
            // the leaves' upper edges, and its box-drawing "shadow" strokes
            // sit darker. The small one stays flat: with only three rows the
            // light steps show as seams inside its strokes.
            let leaves = [Color::hex(0x5ae202), Color::hex(0xff8203), Color::hex(0xffc468)];
            let columns = banner[0].chars().count() as f32 - 1.0;
            let shine = if banner.len() > BANNER.len() { 0.22 } else { 0.0 };
            for (row, line) in banner.iter().enumerate() {
                let light = shine * (1.0 - row as f32 / (banner.len() - 1) as f32);
                for (column, c) in line.chars().enumerate() {
                    let t = column as f32 / columns * 2.0;
                    let leaf =
                        if t < 1.0 { leaves[0].mix_hue(leaves[1], t) } else { leaves[1].mix_hue(leaves[2], t - 1.0) };
                    let color = if matches!(c, '█' | '▀' | '▄') {
                        leaf.mix(Color::hex(0xffffff), light)
                    } else {
                        leaf.mix(theme.background, 0.55)
                    };
                    let cx = x + column as f32 * text.cell.width;
                    text.draw_cell(scene, c, cx, y, Style::default(), color, 1);
                }
                y += text.cell.height;
            }
        } else {
            text.draw_text(scene, "Temino", x, y + 26.0, UI_BOLD, 28.0, theme.text);
            y += 30.0;
        }
        let tagline = "An ADHD-friendly, GPU-accelerated modern terminal, designed for the AI age";
        let width = text.draw_text(scene, tagline, x, y + 22.0, UI, 13.0, theme.text_dim);
        let api = if cfg!(target_os = "macos") { "Metal" } else { "Vulkan" };
        let device = format!("  ·  {api} on {device_name}  ·  v{}", env!("CARGO_PKG_VERSION"));
        text.draw_text(scene, &device, x + width, y + 22.0, UI, 13.0, theme.text_faint);
        let y = y + 36.0;
        let cmd = platform::shortcut;
        let rows: [Vec<(String, &str)>; 3] = [
            vec![
                ("Enter".into(), "run"),
                ("Shift+Enter".into(), "new line"),
                ("Tab".into(), "complete · AI on empty"),
                ("↑ ↓".into(), "history"),
                ("Ctrl+R".into(), "search history"),
            ],
            vec![
                (cmd("T"), "new tab"),
                (cmd("D"), "split right"),
                (cmd("E"), "split down"),
                (cmd("[ ]"), "switch pane"),
                (cmd("W"), "close"),
            ],
            vec![
                (cmd("K"), "clear"),
                (cmd("↑ ↓"), "select blocks"),
                (cmd("C"), "copy"),
                (cmd("V"), "paste"),
                (cmd("+ −"), "font size"),
            ],
        ];
        // In columns, each as wide as its widest hint, where the pane has
        // the room; else each row runs on by itself.
        let mut columns = [0.0f32; 5];
        for hints in &rows {
            for (column, (key, label)) in hints.iter().enumerate() {
                columns[column] = columns[column].max(ui::hint_width(text, key, label));
            }
        }
        const GAP: f32 = 22.0;
        let grid = x + columns.iter().sum::<f32>() + 4.0 * GAP <= self.rect.w - PAD_X;
        for (row, hints) in rows.iter().enumerate() {
            let mut hx = x;
            let hy = y + row as f32 * 28.0;
            for (column, (key, label)) in hints.iter().enumerate() {
                let w = ui::hint(scene, text, theme, key, label, hx, hy);
                hx += if grid { columns[column] + GAP } else { w + 18.0 };
            }
        }
    }
    pub(super) fn draw_block(
        &mut self,
        scene: &mut Scene,
        ctx: &mut Ctx,
        focused: bool,
        index: usize,
        y: f32,
        block_height: f32,
        output: usize,
    ) {
        let width = self.rect.w;
        let pane_height = self.rect.h;
        let cell = ctx.text.cell;
        let blink = ctx.blink_on();
        let id = self.session.blocks[index].id;
        let selection = self.selection_span(id);
        let command_selection = self.command_span(id);
        let selected = self.selected == Some(id);
        let user_columns = self.session.size.columns.max(1);
        let Ctx { text, theme, .. } = ctx;
        let block = &self.session.blocks[index];
        let rect = Rect::new(0.0, y, width, block_height - 1.0);

        if selected {
            scene.fill(rect, theme.accent.alpha(0.07));
            scene.fill(Rect::new(0.0, y, 3.0, rect.h), theme.accent);
        } else if block.failed() {
            scene.fill(rect, theme.error.alpha(0.055));
            scene.fill(Rect::new(0.0, y, 3.0, rect.h), theme.error.alpha(0.8));
        }

        // Meta line: where it ran, and how it went.
        let ascent = text.ui_ascent(12.0);
        let baseline = (y + BLOCK_PAD_TOP + (META_HEIGHT + ascent * 0.72) / 2.0).round();
        let mut x = PAD_X;
        if block.kind == BlockKind::Background {
            text.draw_text(scene, "Background output", x, baseline, UI, 12.0, theme.text_faint);
        } else {
            // How long it took, then how it went — worked out first,
            // because the left side has to stop short of it. A command
            // that went well says so quietly (a faint time, a green
            // check); one still running or failed is in color all through.
            let took = ui::format_duration(block.duration());
            let (lead, lead_ink, tail, tail_ink) = if block.running() {
                let frame = (block.started.elapsed().as_millis() / 80) as usize % SPINNER.len();
                (SPINNER[frame].to_string(), theme.accent, took, theme.accent)
            } else {
                match block.exit {
                    Some(0) | None => (took, theme.text_faint, "✓".to_owned(), theme.success),
                    Some(_) if block.kind == BlockKind::Ai => (took, theme.error, "failed".to_owned(), theme.error),
                    Some(code) => (took, theme.error, format!("exit {code}"), theme.error),
                }
            };
            let gap = text.measure("  ", UI, 12.0);
            let tail_x = width - PAD_X - text.measure(&tail, UI, 12.0);
            let lead_x = tail_x - gap - text.measure(&lead, UI, 12.0);
            // A deep path or a long branch name elides at this limit
            // instead of running under the duration and exit text.
            let limit = lead_x - gap;
            if block.kind == BlockKind::Ai {
                let label = if text.has_glyph('✦', UI_BOLD) { "✦ AI  " } else { "AI  " };
                x += text.draw_text(scene, label, x, baseline, UI_BOLD, 12.0, theme.variable);
            }
            let path = text.elide(&ui::short_path(&block.cwd), UI, 12.0, (limit - x).max(0.0));
            x += text.draw_text(scene, &path, x, baseline, UI, 12.0, theme.text_dim);
            if let Some(branch) = &block.branch {
                x += 10.0;
                x += text.draw_text(scene, "⎇ ", x, baseline, UI, 12.0, theme.variable.alpha(0.9));
                let branch = text.elide(branch, UI, 12.0, (limit - x).max(0.0));
                text.draw_text(scene, &branch, x, baseline, UI, 12.0, theme.variable.alpha(0.9));
            }
            text.draw_text(scene, &tail, tail_x, baseline, UI, 12.0, tail_ink);
            text.draw_text(scene, &lead, lead_x, baseline, UI, 12.0, lead_ink);
        }

        // The command, wrapped to the grid width.
        let command_y = y + BLOCK_PAD_TOP + META_HEIGHT;
        let mut line_y = command_y;
        let mut command_rows_drawn = 0;
        if block.kind != BlockKind::Background {
            for (index, (row, _)) in command_rows(&block.command, user_columns).iter().enumerate() {
                if let Some(((l0, c0), (l1, c1))) =
                    command_selection.filter(|((l0, _), (l1, _))| (*l0..=*l1).contains(&index))
                {
                    let from = if index == l0 { c0 } else { 0 };
                    let to = if index == l1 { c1 } else { usize::MAX };
                    let (start, end) = row_char_range(row, from, to);
                    if end > start {
                        let x = PAD_X + row_cell_of_char(row, start) as f32 * cell.width;
                        let w = (row_cell_of_char(row, end) - row_cell_of_char(row, start)) as f32 * cell.width;
                        scene.fill(Rect::new(x, line_y, w, cell.height), theme.selection);
                    }
                }
                text.draw_mono(scene, row, PAD_X, line_y, Style::BOLD, theme.text);
                line_y += cell.height;
                command_rows_drawn += 1;
            }
        }

        let output_y = line_y + OUTPUT_GAP;
        if output > 0 {
            // Only the visible slice of long output is drawn.
            let first = ((-output_y) / cell.height).floor().max(0.0) as usize;
            let last = (((pane_height - output_y) / cell.height).ceil().max(0.0) as usize).min(output);
            if first < last {
                let view = GridView {
                    block,
                    x: PAD_X,
                    y: output_y + first as f32 * cell.height,
                    first,
                    last,
                    cursor: block.running() && block.kind == BlockKind::Command,
                    focused,
                    selection,
                    marks: self.find.as_ref().map(|f| f.marks(id, first, last)).unwrap_or_default(),
                    blink_on: blink,
                };
                ui::draw_grid(scene, text, theme, view);
            }
            for placement in block.pictures.iter().filter(|p| p.line < last && p.line + p.rows > first) {
                let room = Rect::new(
                    PAD_X + placement.column as f32 * cell.width,
                    output_y + placement.line as f32 * cell.height,
                    placement.columns as f32 * cell.width,
                    placement.rows as f32 * cell.height,
                );
                let picture = &placement.picture;
                let aspect = picture.height as f32 / picture.width as f32;
                let (w, h) =
                    if room.w * aspect <= room.h { (room.w, room.w * aspect) } else { (room.h / aspect, room.h) };
                text.draw_picture(scene, picture, Rect::new(room.x, room.y, w, h));
            }
        } else if waiting_on_ai(block, output) {
            // Where the answer will go, so it's plain the request was taken.
            let frame = (block.started.elapsed().as_millis() / 80) as usize % SPINNER.len();
            let spinner = SPINNER[frame].to_string();
            text.draw_mono(scene, &spinner, PAD_X, output_y, Style::default(), theme.accent);
            text.draw_mono(scene, "Thinking…", PAD_X + 2.0 * cell.width, output_y, Style::default(), theme.text_dim);
        }
        scene.fill(Rect::new(0.0, y + block_height - 1.0, width, 1.0), theme.divider);
        self.layout.blocks.push(BlockLayout {
            id,
            rect,
            output_x: PAD_X,
            output_y,
            lines: output,
            command_y,
            command_rows: command_rows_drawn,
        });
    }
    pub(super) fn draw_input(&mut self, scene: &mut Scene, ctx: &mut Ctx, focused: bool, y: f32, height: f32) {
        let width = self.rect.w;
        let cell = ctx.text.cell;
        let blink = ctx.blink_on() && focused;
        let phase = self.session.phase;
        let hint = if self.completion.is_none() { self.suggestion(ctx) } else { None };
        // Coming back to the pane, its recap takes the hint's place (an
        // agent's goes in its header instead).
        let recap = self.recap.as_ref().filter(|_| self.agent_now.is_none()).map(Recap::line);
        let Ctx { text, theme, commands, user_host, .. } = ctx;
        let Pane { session, editor, layout, completion, ai, .. } = self;
        let ai = *ai && phase == Phase::Ready;
        let panel = Rect::new(0.0, y, width, height);
        layout.input = panel;
        scene.fill(panel, theme.panel(SURFACE_LIFT));
        scene.fill(Rect::new(0.0, y, width, 1.0), theme.divider);
        // With room left under it, an edge there too: a band, not a step.
        if panel.bottom() < self.rect.h - 1.0 {
            scene.fill(Rect::new(0.0, panel.bottom() - 1.0, width, 1.0), theme.divider);
        }

        // Context chips.
        let chip_y = y + INPUT_PAD;
        let mut x = PAD_X;
        x += ui::chip(scene, text, &ui::short_path(&session.cwd), x, chip_y, CHIP_HEIGHT, theme.raised, theme.accent)
            + 6.0;
        if let Some(branch) = &session.branch {
            x += ui::chip(scene, text, &format!("⎇ {branch}"), x, chip_y, CHIP_HEIGHT, theme.raised, theme.variable)
                + 6.0;
        }
        ui::chip(scene, text, user_host, x, chip_y, CHIP_HEIGHT, theme.raised, theme.text_dim);

        let editor_y = chip_y + CHIP_HEIGHT + 8.0;
        let text_x = PAD_X + 2.0 * cell.width;
        layout.editor_x = text_x;
        layout.editor_y = editor_y;
        // Not ready for a command yet (starting, or one still running): a
        // dim hourglass instead of the prompt, so it doesn't look ready.
        let busy = matches!(phase, Phase::Boot | Phase::Running | Phase::Submitted | Phase::Prompt);
        let prompt_color = match phase {
            _ if busy => theme.text_faint,
            Phase::Continuation => theme.warning,
            _ if !focused => theme.text_faint,
            _ if ai => theme.variable,
            _ => theme.accent,
        };
        let prompt = match (ai, text.has_glyph('❯', 0)) {
            _ if busy && text.has_glyph('⧗', 0) => "⧗",
            _ if busy => "…",
            (true, _) if text.has_glyph('✦', 0) => "✦",
            (true, _) => "?",
            (false, true) => "❯",
            (false, false) => ">",
        };
        text.draw_mono(scene, prompt, PAD_X, editor_y, Style::BOLD, prompt_color);

        let status = |scene: &mut Scene, text: &mut TextSystem, message: &str, color: Color| {
            let baseline = editor_y + text.cell.baseline;
            let message = text.elide(message, UI, 13.0, width - text_x - PAD_X);
            text.draw_text(scene, &message, text_x, baseline, UI, 13.0, color);
        };
        match phase {
            Phase::Boot => status(scene, text, "Starting bash…", theme.text_faint),
            Phase::Exited => {
                let code = session.exit_code.unwrap_or(0);
                status(
                    scene,
                    text,
                    &format!("The shell exited (status {code}). Press Enter to start a new one."),
                    theme.text_dim,
                )
            }
            Phase::Running => match &recap {
                Some(recap) => status(scene, text, recap, theme.text_dim),
                None => {
                    let message = format!(
                        "Running · keystrokes go to the program · Ctrl+C interrupts · {} copies",
                        platform::shortcut("C")
                    );
                    status(scene, text, &message, theme.text_faint)
                }
            },
            Phase::Prompt | Phase::Submitted => {}
            Phase::Ready | Phase::Continuation => {
                let first_line = editor.position(editor.cursor).0.saturating_sub(MAX_EDITOR_LINES - 1);
                if editor.text.is_empty() && !ctx.composing {
                    let hint = if phase == Phase::Continuation {
                        "The command needs more input; Ctrl+C cancels it"
                    } else if ai {
                        "Ask AI · describe a command, or ask anything · Tab back to the shell"
                    } else {
                        "Type a command · Tab here for AI · Tab completes · ↑ history"
                    };
                    match &recap {
                        Some(recap) if phase == Phase::Ready && !ai => status(scene, text, recap, theme.text_dim),
                        _ => status(scene, text, hint, theme.text_faint),
                    }
                }
                // Selection.
                if let Some((a, b)) = editor.selection() {
                    let (l0, c0) = editor.position(a);
                    let (l1, c1) = editor.position(b);
                    for line in l0..=l1 {
                        if line < first_line || line >= first_line + MAX_EDITOR_LINES {
                            continue;
                        }
                        let from = if line == l0 { c0 } else { 0 };
                        let to = if line == l1 {
                            c1
                        } else {
                            editor.text.split('\n').nth(line).map_or(0, crate::editor::columns) + 1
                        };
                        let ly = editor_y + (line - first_line) as f32 * cell.height;
                        let rect = Rect::new(
                            text_x + from as f32 * cell.width,
                            ly,
                            (to.saturating_sub(from)) as f32 * cell.width,
                            cell.height,
                        );
                        scene.fill(rect, theme.selection);
                    }
                }
                // Text with syntax colors.
                commands.use_path(self.session.path.as_deref());
                let spans = if ai { Vec::new() } else { ui::highlight(&editor.text, editor.cursor, commands, theme) };
                let mut colors = vec![theme.text; editor.text.len()];
                for (start, end, color) in spans {
                    for slot in &mut colors[start..end.min(editor.text.len())] {
                        *slot = color;
                    }
                }
                let (mut line, mut column) = (0usize, 0usize);
                for (offset, c) in editor.text.char_indices() {
                    if c == '\n' {
                        line += 1;
                        column = 0;
                        continue;
                    }
                    let span = crate::editor::width(c);
                    if line >= first_line && line < first_line + MAX_EDITOR_LINES && span > 0 {
                        let cx = text_x + column as f32 * cell.width;
                        let cy = editor_y + (line - first_line) as f32 * cell.height;
                        text.draw_cell(scene, c, cx, cy, Style::default(), colors[offset], span);
                    }
                    column += span;
                }
                if let Some(rest) = hint.as_deref() {
                    let (sl, sc) = editor.position(editor.text.len());
                    if sl >= first_line {
                        let sy = editor_y + (sl - first_line) as f32 * cell.height;
                        text.draw_mono(
                            scene,
                            rest,
                            text_x + sc as f32 * cell.width,
                            sy,
                            Style::default(),
                            theme.text_faint,
                        );
                    }
                }
                let (cl, cc) = editor.position(editor.cursor);
                if cl >= first_line {
                    let cy = editor_y + (cl - first_line) as f32 * cell.height;
                    let caret = Rect::new(text_x + cc as f32 * cell.width - 1.0, cy + 1.0, 2.0, cell.height - 2.0);
                    if focused {
                        scene.mark_caret(Rect::new(caret.x + 1.0, cy, cell.width, cell.height));
                    }
                    if blink {
                        scene.fill(caret, theme.cursor);
                    }
                }
                if let Some(completion) = completion {
                    let (_, word_column) = editor.position(completion.start.min(editor.text.len()));
                    draw_menu(
                        scene,
                        text,
                        theme,
                        layout,
                        completion,
                        width,
                        text_x + word_column as f32 * cell.width,
                        editor_y,
                    );
                }
            }
        }
    }
}

pub(super) fn draw_menu(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    layout: &mut Layout,
    completion: &Completion,
    pane_width: f32,
    x: f32,
    editor_y: f32,
) {
    let cell = text.cell;
    scene.set_layer(LAYER_OVERLAY);
    let rows = completion.candidates.len().min(MENU_ROWS);
    let row_height = cell.height + 6.0;
    let first = completion.selected.saturating_sub(rows - 1);
    // Labels are drawn with draw_mono, which gives a wide character two
    // cells; measure them in cells, not characters, or a CJK file name
    // runs under the kind tag at the menu's right edge.
    let label_width = completion.candidates[first..first + rows]
        .iter()
        .map(|c| c.label.chars().take(80).map(crate::editor::width).sum::<usize>() as f32 * cell.width)
        .fold(0.0, f32::max);
    let width = (label_width + 90.0).clamp(220.0, (pane_width - 2.0 * PAD_X).max(220.0));
    let footer = if completion.candidates.len() > rows { 18.0 } else { 0.0 };
    let height = rows as f32 * row_height + 8.0 + footer;
    let x = (x - 10.0).min(pane_width - width - PAD_X).max(PAD_X);
    let y = (editor_y - height - 10.0).max(4.0);
    let rect = Rect::new(x, y, width, height);
    scene.shadow(rect, theme.shadow(0.55), 8.0, 10.0);
    scene.quad(Quad::new(rect, theme.raised).radius(8.0).border(1.0, theme.divider));
    scene.push_clip(rect.inset(4.0, 4.0));
    for row in 0..rows {
        let item = first + row;
        let candidate = &completion.candidates[item];
        let ry = y + 4.0 + row as f32 * row_height;
        let row_rect = Rect::new(x + 4.0, ry, width - 8.0, row_height);
        if item == completion.selected {
            scene.quad(Quad::new(row_rect, theme.accent.alpha(0.22)).radius(5.0));
        }
        let color = match candidate.kind {
            Kind::Directory => theme.accent,
            Kind::Command => theme.success,
            _ => theme.text,
        };
        let label: String = candidate.label.chars().take(80).collect();
        text.draw_mono(scene, &label, x + 12.0, ry + 3.0, Style::default(), color);
        let tag = match candidate.kind {
            Kind::Command => "command",
            Kind::Directory => "folder",
            Kind::File => "file",
            Kind::History => "history",
        };
        let tag_width = text.measure(tag, UI, 11.0);
        text.draw_text(
            scene,
            tag,
            x + width - tag_width - 12.0,
            ry + 3.0 + text.cell.baseline - 1.0,
            UI,
            11.0,
            theme.text_faint,
        );
        layout.menu_rows.push((row_rect, item));
    }
    if completion.candidates.len() > rows {
        let more = format!("{} of {}", completion.selected + 1, completion.candidates.len());
        let w = text.measure(&more, UI, 10.0);
        text.draw_text(scene, &more, x + width - w - 12.0, y + height - 8.0, UI, 10.0, theme.text_faint);
    }
    scene.pop_clip();
    scene.set_layer(LAYER_CONTENT);
}

/// A command's rows as drawn, wrapped to the grid width; the flag marks
/// the last row of each of its lines.
/// An AI request with no answer yet: its block shows a spinner in the answer's place.
pub(super) fn waiting_on_ai(block: &Block, output: usize) -> bool {
    block.kind == BlockKind::Ai && block.running() && output == 0
}

pub(super) fn command_rows(command: &str, columns: usize) -> Vec<(String, bool)> {
    let mut rows = Vec::new();
    for line in command.split('\n') {
        let mut row = String::new();
        let mut used = 0;
        for c in line.chars() {
            let w = crate::editor::width(c);
            // Wrap on cells, the grid's own measure: a wide character that
            // would straddle the edge starts the next row instead.
            if w > 0 && used > 0 && used + w > columns {
                rows.push((std::mem::take(&mut row), false));
                used = 0;
            }
            row.push(c);
            used += w;
        }
        rows.push((row, true));
    }
    rows
}

/// The char range of a wrapped command row covering cells `from..=to`
/// (clamped to the row): selection spots are in cells, the row's text is
/// in chars, and a wide character covers two cells.
pub(super) fn row_char_range(row: &str, from: usize, to: usize) -> (usize, usize) {
    let (mut start, mut end, mut used) = (None, 0, 0);
    for (index, c) in row.chars().enumerate() {
        let w = crate::editor::width(c);
        if start.is_none() && used + w > from {
            start = Some(index);
        }
        if used <= to {
            end = index + 1;
        }
        used += w;
    }
    (start.unwrap_or(end), end)
}

/// The cell a char index starts at in a wrapped command row.
pub(super) fn row_cell_of_char(row: &str, char_index: usize) -> usize {
    row.chars().take(char_index).map(crate::editor::width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_rows_wrap_on_cells_not_chars() {
        // ASCII is unchanged: chunks of `columns` characters.
        let rows = command_rows("abcdefg", 3);
        assert_eq!(rows, vec![("abc".into(), false), ("def".into(), false), ("g".into(), true)]);
        // Wide characters take two cells each, so fewer fit a row.
        let rows = command_rows("ab中文cd", 4);
        assert_eq!(rows, vec![("ab中".into(), false), ("文cd".into(), true)]);
        // A wide character that would straddle the edge wraps whole.
        let rows = command_rows("abc中", 4);
        assert_eq!(rows, vec![("abc".into(), false), ("中".into(), true)]);
        // Empty lines still take a row, flagged as ending their line.
        let rows = command_rows("a\n\nb", 5);
        assert_eq!(rows, vec![("a".into(), true), ("".into(), true), ("b".into(), true)]);
    }

    #[test]
    fn the_completion_menu_fits_wide_labels() {
        use crate::complete::{Candidate, Completion, Kind};
        let mut scene = Scene::new();
        let mut text = TextSystem::new(14.0);
        let theme = Theme::dark();
        // Ten wide characters: ten chars, twenty cells.
        let completion = Completion {
            start: 0,
            end: 0,
            selected: 0,
            candidates: vec![Candidate {
                replacement: String::new(),
                label: "中文中文中文中文中文".into(),
                kind: Kind::File,
            }],
        };
        let mut layout = Layout::default();
        draw_menu(&mut scene, &mut text, &theme, &mut layout, &completion, 800.0, 100.0, 400.0);
        let (row, _) = layout.menu_rows[0];
        // The row must leave room for the label's 20 cells plus the tag.
        assert!(row.w + 8.0 >= 20.0 * text.cell.width + 90.0, "menu width {} for a 20-cell label", row.w + 8.0);
    }

    #[test]
    fn row_char_range_maps_cells_to_chars() {
        // Cells: a = 0, 中 = 1..=2, b = 3.
        let row = "a中b";
        assert_eq!(row_char_range(row, 0, 0), (0, 1));
        assert_eq!(row_char_range(row, 1, 2), (1, 2));
        assert_eq!(row_char_range(row, 2, 2), (1, 2), "a cell inside a wide char is that char");
        assert_eq!(row_char_range(row, 3, 3), (2, 3));
        assert_eq!(row_char_range(row, 0, 99), (0, 3), "past the end clamps");
        assert_eq!(row_cell_of_char(row, 2), 3);
        assert_eq!(row_cell_of_char(row, 0), 0);
    }
}

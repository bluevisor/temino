use super::*;
use crate::Locked;

impl Pane {
    pub fn key(&mut self, ctx: &mut Ctx, key: KeyEvent) {
        if !key.cmd() {
            self.recap = None;
        }
        let named = keys::named(&key);
        if self.find_key(&key, named) {
            return;
        }
        // At the prompt, ⌘←/→ and ⌘⌫ are line editing (see editor_key).
        let line_edit = key.line_mod()
            && self.session.editor_active()
            && matches!(named, Some(Named::Left | Named::Right | Named::Backspace));
        // Command shortcuts work in every mode.
        if key.cmd() && !key.ctrl() && !line_edit {
            let handled = match (named, key.base_char()) {
                (Some(Named::Up), _) => {
                    self.select_neighbor(-1);
                    true
                }
                (Some(Named::Down), _) => {
                    self.select_neighbor(1);
                    true
                }
                (_, Some('c')) => {
                    self.copy(ctx, key.shift());
                    true
                }
                (_, Some('v')) => {
                    self.paste(platform::clipboard_get());
                    true
                }
                (_, Some('k')) => {
                    self.clear_blocks();
                    true
                }
                (_, Some('f')) => {
                    self.open_find();
                    true
                }
                (_, Some('g')) => {
                    // Find Next with the bar closed: the last search again.
                    self.open_find();
                    if let Some(find) = self.find.as_mut() {
                        find.selected = false;
                    }
                    true
                }
                (_, Some('a')) if self.session.editor_active() => {
                    self.editor.select_all();
                    true
                }
                (_, Some('z')) if self.session.editor_active() => {
                    self.editor.undo();
                    true
                }
                _ => false,
            };
            // Off Haiku, Command is never Meta: an unbound shortcut does nothing.
            if handled || !cfg!(target_os = "haiku") {
                return;
            }
        }

        if let Some(block) = self.session.active() {
            let alt = block.alt_screen();
            let mode = *block.term.mode();
            if !alt && key.shift() && matches!(named, Some(Named::PageUp | Named::PageDown)) {
                let page = self.layout.list.h * 0.8;
                self.scroll_by(if named == Some(Named::PageUp) { page } else { -page });
                return;
            }
            let bytes = keys::encode(&key, mode);
            if !bytes.is_empty() {
                self.track_typing(&String::from_utf8_lossy(&bytes));
                self.session.write(&bytes);
                self.selection = None;
                self.pin_to_bottom();
            }
            return;
        }

        match self.session.phase {
            Phase::Ready | Phase::Continuation => self.editor_key(ctx, key, named),
            Phase::Exited => {
                if named == Some(Named::Enter) {
                    self.restart(ctx);
                }
            }
            _ => {
                if key.ctrl() && key.base_char() == Some('c') {
                    self.session.interrupt();
                }
            }
        }
    }
    /// Command+F: the search bar, or its query selected if it's open.
    pub fn open_find(&mut self) {
        match &mut self.find {
            Some(find) => find.selected = !find.query.is_empty(),
            None => self.find = Some(Find::new(self.last_query.clone())),
        }
    }
    /// TEMINO_FIND_TEST: the search bar open on `query`, for screenshots.
    pub fn find_test(&mut self, query: &str) {
        let mut find = Find::new(query.to_owned());
        find.selected = false;
        self.find = Some(find);
    }
    /// TEMINO_ASK_TEST: `request` asked in AI mode, for screenshots.
    pub fn ask_test(&mut self, ctx: &mut Ctx, request: &str) {
        self.editor.take();
        self.editor.insert(request);
        self.ask(ctx);
    }
    pub fn close_find(&mut self) {
        if let Some(find) = self.find.take() {
            self.last_query = find.query;
        }
    }
    /// A key while the search bar is open; false lets it through.
    pub(super) fn find_key(&mut self, key: &KeyEvent, named: Option<Named>) -> bool {
        let Some(find) = self.find.as_mut() else { return false };
        if key.cmd() && !key.ctrl() {
            match key.base_char() {
                Some('g') => find.step(!key.shift()),
                Some('a') => find.selected = !find.query.is_empty(),
                Some('v') => {
                    let pasted = platform::clipboard_get().unwrap_or_default();
                    if find.selected {
                        find.query.clear();
                    }
                    find.query.push_str(pasted.lines().next().unwrap_or(""));
                    find.selected = false;
                }
                Some('f') => find.selected = !find.query.is_empty(),
                _ => return false,
            }
            return true;
        }
        match named {
            Some(Named::Escape) => self.close_find(),
            Some(Named::Enter) => find.step(!key.shift()),
            Some(Named::Up) => find.step(true),
            Some(Named::Down) => find.step(false),
            Some(Named::Backspace) if find.selected => {
                find.query.clear();
                find.selected = false;
            }
            Some(Named::Backspace) => {
                find.query.pop();
            }
            Some(Named::PageUp | Named::PageDown) => return false,
            Some(_) => {}
            None if key.ctrl() => {
                if key.base_char() == Some('c') || key.base_char() == Some('g') {
                    self.close_find();
                }
            }
            None => {
                let typed: String = String::from_utf8_lossy(&key.bytes).chars().filter(|c| !c.is_control()).collect();
                if !typed.is_empty() {
                    if find.selected {
                        find.query.clear();
                        find.selected = false;
                    }
                    find.query.push_str(&typed);
                }
            }
        }
        true
    }
    /// The search bar, over the pane's top-right corner.
    pub(super) fn draw_find(&mut self, scene: &mut Scene, ctx: &mut Ctx) {
        let Some(find) = &self.find else { return };
        let Ctx { text, theme, .. } = ctx;
        let font = 12.5;
        let width = (self.rect.w - 16.0).min(340.0);
        let bar = Rect::new(self.rect.w - width - 8.0, 6.0, width, 30.0);
        scene.set_layer(LAYER_OVERLAY);
        scene.shadow(bar, Color::hex(0x000000).alpha(0.4), 8.0, 6.0);
        scene.quad(Quad::new(bar, theme.background).radius(8.0));
        scene.quad(Quad::new(bar, theme.raised).radius(8.0).border(1.0, theme.accent.alpha(0.6)));
        let baseline = (bar.y + (bar.h + text.ui_ascent(font) * 0.72) / 2.0).round();
        let status = find.status();
        let status_w = text.measure(&status, UI, 11.5);
        let status_color = if find.current.is_none() { theme.error } else { theme.text_dim };
        text.draw_text(scene, &status, bar.right() - 12.0 - status_w, baseline, UI, 11.5, status_color);
        let x = bar.x + 12.0;
        let room = (bar.w - 36.0 - status_w).max(0.0);
        scene.push_clip(Rect::new(x - 2.0, bar.y, room + 4.0, bar.h));
        if find.query.is_empty() {
            text.draw_text(scene, "Find in scrollback", x, baseline, UI, font, theme.text_faint);
            scene.fill(Rect::new(x, bar.y + 8.0, 1.0, bar.h - 16.0), theme.cursor);
        } else {
            let w = text.measure(&find.query, UI, font);
            let x = x - (w - room + 4.0).max(0.0);
            if find.selected {
                scene.quad(
                    Quad::new(Rect::new(x - 1.0, bar.y + 7.0, w + 2.0, bar.h - 14.0), theme.selection).radius(2.0),
                );
            }
            text.draw_text(scene, &find.query, x, baseline, UI, font, theme.text);
            scene.fill(Rect::new(x + w + 1.0, bar.y + 8.0, 1.0, bar.h - 16.0), theme.cursor);
        }
        scene.pop_clip();
        scene.set_layer(LAYER_CONTENT);
    }
    pub(super) fn restart(&mut self, ctx: &mut Ctx) {
        self.session = Session::new(grid_size(self.rect, &ctx.text));
        self.session.set_cell_size(ctx.text.cell.width, ctx.text.cell.height);
        self.credits = Credits::new();
        if let Err(error) = self.session.start(self.id, &ctx.shell, ctx.sender.clone(), self.credits.clone()) {
            ctx.show_toast(&format!("Couldn't start the shell: {error}"));
        }
        self.selection = None;
        self.selected = None;
    }
    pub(super) fn editor_key(&mut self, ctx: &mut Ctx, key: KeyEvent, named: Option<Named>) {
        let shift = key.shift();
        if self.completion.is_some() {
            match named {
                Some(Named::Up) => return self.move_menu(-1),
                Some(Named::Down) => return self.move_menu(1),
                Some(Named::Tab) => return self.move_menu(if shift { -1 } else { 1 }),
                Some(Named::Enter) => return self.accept_completion(),
                Some(Named::Escape) => {
                    self.completion = None;
                    return;
                }
                Some(Named::Left | Named::Right | Named::Home | Named::End) => self.completion = None,
                _ => {}
            }
        }
        // Which menu was open before this key, so typing can refilter it.
        let open_menu = self.completion.as_ref().and_then(|c| c.candidates.first()).map(|f| f.kind == Kind::History);
        let suggestion = self.suggestion(ctx);
        let word = key.ctrl() || key.word_mod();
        let line = key.line_mod();
        match named {
            Some(Named::Enter) => {
                if shift || key.option() {
                    self.editor.insert("\n");
                } else if self.ai {
                    self.ask(ctx);
                } else {
                    self.submit(ctx);
                }
            }
            Some(Named::Backspace) => {
                if line {
                    self.editor.kill_to_start()
                } else if word {
                    self.editor.delete_word_back()
                } else {
                    self.editor.backspace()
                }
            }
            Some(Named::Delete) => self.editor.delete(),
            Some(Named::Left) if line => self.editor.home(shift),
            Some(Named::Left) => self.editor.left(word, shift),
            // Like End: at the end of the line it takes the whole suggestion.
            Some(Named::Right) if line => match suggestion {
                Some(rest) if !shift => self.editor.insert(&rest),
                _ => self.editor.end(shift),
            },
            Some(Named::Right) => match suggestion {
                Some(rest) if !shift => {
                    let take = if word { rest.split_inclusive(' ').next().unwrap_or(&rest).to_owned() } else { rest };
                    self.editor.insert(&take);
                }
                _ => self.editor.right(word, shift),
            },
            Some(Named::Home) => self.editor.home(shift),
            Some(Named::End) => match suggestion {
                Some(rest) if !shift => self.editor.insert(&rest),
                _ => self.editor.end(shift),
            },
            Some(Named::Up) => {
                if !self.editor.vertical(false, shift) {
                    self.editor.browse(&ctx.history, true);
                }
            }
            Some(Named::Down) => {
                if !self.editor.vertical(true, shift) {
                    self.editor.browse(&ctx.history, false);
                }
            }
            // Tab on an empty line switches between the shell and AI mode;
            // after text it takes a grey history suggestion whole, or completes.
            Some(Named::Tab) if self.editor.text.is_empty() && self.session.phase == Phase::Ready => {
                self.ai = !self.ai;
                self.hint = None;
            }
            Some(Named::Tab) if self.ai => {}
            Some(Named::Tab) => match self.editor.suggestion(&ctx.history).map(str::to_owned) {
                Some(rest) if !shift => self.editor.insert(&rest),
                _ => self.tab(ctx),
            },
            Some(Named::Escape) if self.ai && self.editor.text.is_empty() => self.ai = false,
            Some(Named::Escape) => {
                self.selected = None;
                self.selection = None;
                self.editor.anchor = None;
            }
            Some(Named::PageUp) => self.scroll_by(self.layout.list.h * 0.8),
            Some(Named::PageDown) => self.scroll_by(-self.layout.list.h * 0.8),
            Some(Named::Insert | Named::F(_)) => {}
            None => {
                if key.ctrl() {
                    match key.base_char() {
                        Some('c') => {
                            if self.session.phase == Phase::Continuation {
                                self.session.interrupt();
                            }
                            self.editor.clear();
                            self.completion = None;
                        }
                        Some('a') => self.editor.home(shift),
                        Some('e') => match suggestion {
                            Some(rest) => self.editor.insert(&rest),
                            None => self.editor.end(shift),
                        },
                        Some('u') => self.editor.kill_to_start(),
                        Some('k') => self.editor.kill_to_end(),
                        Some('w') => self.editor.delete_word_back(),
                        Some('l') => self.clear_blocks(),
                        Some('z') => self.editor.undo(),
                        Some('r') => self.open_history_menu(ctx),
                        // Every command, or what completes the word: Tab's
                        // menu, now that Tab on an empty line means AI mode.
                        Some(' ') if shift && !self.ai => self.tab(ctx),
                        Some('d') => {
                            if self.editor.text.is_empty() {
                                self.session.submit("exit");
                            } else {
                                self.editor.delete();
                            }
                        }
                        _ => {}
                    }
                } else if key.cmd() {
                    // Unbound Command combinations: nothing at the prompt.
                } else if let Ok(text) = std::str::from_utf8(&key.bytes) {
                    if !text.is_empty() && !text.chars().any(char::is_control) {
                        self.editor.insert(text);
                        self.selected = None;
                    }
                }
            }
        }
        // Keep an open menu in step with what was typed.
        let edited = matches!(named, None | Some(Named::Backspace | Named::Delete)) && !key.ctrl();
        if let (Some(history), true) = (open_menu, edited && self.completion.is_some()) {
            if history {
                self.open_history_menu(ctx);
            } else {
                ctx.commands.use_path(self.session.path.as_deref());
                self.completion =
                    complete::complete(&self.editor.text, self.editor.cursor, &self.session.cwd, &mut ctx.commands)
                        .filter(|c| c.candidates.len() > 1);
            }
        }
    }
    /// The grey text after the caret: the newest history entry that starts
    /// with what's typed, or else what Tab would fill in for the last word.
    pub(super) fn suggestion(&mut self, ctx: &mut Ctx) -> Option<String> {
        if self.ai {
            return None;
        }
        if let Some(rest) = self.editor.suggestion(&ctx.history) {
            return Some(rest.to_owned());
        }
        let text = &self.editor.text;
        if text.trim().is_empty()
            || text.ends_with(char::is_whitespace)
            || self.editor.cursor != text.len()
            || text.contains('\n')
        {
            return None;
        }
        let key = format!("{}\0{text}", self.session.cwd);
        if let Some((cached, hint)) = &self.hint {
            if *cached == key {
                return hint.clone();
            }
        }
        ctx.commands.use_path(self.session.path.as_deref());
        let hint = complete::hint(text, &self.session.cwd, &mut ctx.commands);
        self.hint = Some((key, hint.clone()));
        hint
    }
    pub(super) fn submit(&mut self, ctx: &mut Ctx) {
        let command = self.editor.text.trim_end().to_owned();
        self.completion = None;
        if command.trim().is_empty() {
            return;
        }
        ctx.history.push(&command);
        self.hint = None;
        self.editor.take();
        match self.shell_swap(&command) {
            Some((name, line)) => {
                self.session.submit_as(&command, &line);
                self.session.shell_name = name;
            }
            None => self.session.submit(&command),
        }
        self.selected = None;
        self.selection = None;
        self.pin_to_bottom();
    }
    /// Typing just a shell's name (`fish`, or `exec fish`) at the prompt
    /// switches this pane to that shell: it replaces the running one in
    /// place, with Temino's integration, instead of nesting inside a block.
    /// Returns the shell's name and the line that does it.
    pub(super) fn shell_swap(&self, command: &str) -> Option<(String, String)> {
        if self.session.phase == Phase::Continuation {
            return None;
        }
        let words: Vec<&str> = command.split_whitespace().collect();
        let word = match words[..] {
            [word] | ["exec", word] => word,
            _ => return None,
        };
        let kind = crate::shells::Kind::of_path(word)?;
        let path = if word.contains('/') { word.to_owned() } else { crate::shells::find(kind)? };
        let line = crate::session::exec_line(&path).ok()?;
        Some((kind.name().to_owned(), line))
    }
    /// AI mode's Enter: the request goes to the model in a block of its own.
    pub(super) fn ask(&mut self, ctx: &mut Ctx) {
        let request = self.editor.text.trim().to_owned();
        self.completion = None;
        if request.is_empty() || self.asking.is_some() {
            return;
        }
        self.editor.take();
        let id = self.session.ask(&request);
        self.selected = None;
        self.selection = None;
        self.pin_to_bottom();
        if !llm::enabled() {
            self.session.answer(id, "AI mode needs a model server: set llm_url in the settings file.", false);
            return;
        }
        let recent: Vec<String> = self
            .session
            .blocks
            .iter()
            .rev()
            .filter(|b| b.kind == BlockKind::Command && !b.command.trim().is_empty())
            .take(3)
            .map(|b| agent::one_line(&b.command))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let prompt = ask::prompt(&request, &ctx.shell, &self.session.cwd, &recent);
        let reply = Arc::new(Mutex::new(None));
        let theirs = reply.clone();
        let _ = std::thread::Builder::new().name("ask".into()).spawn(move || {
            *theirs.locked() = Some(ask::run(&request, &prompt));
        });
        self.asking = Some((id, reply));
    }
    /// Shows the model's reply once it's in: a command goes into the editor
    /// to look over (Enter runs it, back in shell mode), an answer into its block.
    pub(super) fn take_answer(&mut self) -> bool {
        let Some((id, reply)) = &self.asking else { return false };
        let Some(reply) = reply.locked().take() else { return false };
        let id = *id;
        self.asking = None;
        match reply {
            Ok(Reply::Command(command)) => {
                let placed = self.editor.text.is_empty();
                let note = if placed { "Enter runs it, or edit it first" } else { "copy it from here" };
                self.session.answer(id, &format!("{command}\n\n{note}"), true);
                if placed {
                    self.editor.insert(&command);
                    self.ai = false;
                    self.hint = None;
                }
            }
            Ok(Reply::Answer(answer)) if !answer.is_empty() => self.session.answer(id, &answer, true),
            Ok(Reply::Answer(_)) => self.session.answer(id, "The model gave no answer.", false),
            Err(error) => self.session.answer(id, &format!("The model couldn't be reached: {error}"), false),
        }
        self.pin_to_bottom();
        true
    }
    pub(super) fn tab(&mut self, ctx: &mut Ctx) {
        ctx.commands.use_path(self.session.path.as_deref());
        let Some(mut completion) =
            complete::complete(&self.editor.text, self.editor.cursor, &self.session.cwd, &mut ctx.commands)
        else {
            platform::beep();
            return;
        };
        if completion.candidates.len() == 1 {
            let only = completion.candidates.remove(0);
            self.replace_word(completion.start, completion.end, &only.replacement);
            return;
        }
        let prefix = complete::common_prefix(&completion.candidates);
        let current = &self.editor.text[completion.start..completion.end];
        if prefix.len() > current.len() {
            self.replace_word(completion.start, completion.end, &prefix);
            completion.end = completion.start + prefix.len();
        }
        self.completion = Some(completion);
    }
    pub(super) fn open_history_menu(&mut self, ctx: &Ctx) {
        let query = self.editor.text.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let candidates: Vec<Candidate> = ctx
            .history
            .entries
            .iter()
            .rev()
            .filter(|entry| entry.to_lowercase().contains(&query) && seen.insert(entry.as_str()))
            .take(200)
            .map(|entry| Candidate {
                replacement: entry.clone(),
                label: entry.replace('\n', " ⏎ "),
                kind: Kind::History,
            })
            .collect();
        self.completion = (!candidates.is_empty()).then_some(Completion {
            start: 0,
            end: self.editor.text.len(),
            candidates,
            selected: 0,
        });
    }
    pub(super) fn replace_word(&mut self, start: usize, end: usize, with: &str) {
        self.editor.anchor = Some(start);
        self.editor.cursor = end;
        self.editor.insert(with);
    }
    pub(super) fn move_menu(&mut self, delta: i32) {
        if let Some(completion) = &mut self.completion {
            let n = completion.candidates.len() as i32;
            completion.selected = ((completion.selected as i32 + delta).rem_euclid(n)) as usize;
        }
    }
    pub(super) fn accept_completion(&mut self) {
        if let Some(completion) = self.completion.take() {
            let candidate = &completion.candidates[completion.selected];
            if candidate.kind == Kind::History {
                self.editor.set(&candidate.replacement);
            } else {
                let end = completion.end.min(self.editor.text.len());
                self.replace_word(completion.start, end, &candidate.replacement);
            }
        }
    }
    pub(super) fn clear_blocks(&mut self) {
        self.session.clear();
        self.selected = None;
        self.selection = None;
        self.scroll = 0.0;
        self.scroll_target = 0.0;
        self.welcome = false;
    }
}

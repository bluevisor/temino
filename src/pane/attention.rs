use super::*;

impl Recap {
    pub(super) fn summary(&self) -> Option<Option<String>> {
        self.summary.lock().unwrap().clone()
    }

    /// The recap in one line: the summary once the model wrote it, else
    /// the facts.
    pub(super) fn line(&self) -> String {
        match self.summary() {
            Some(Some(summary)) => format!("✦ {summary}"),
            None => format!("{} · ✦ Summarizing…", self.facts),
            Some(None) => self.facts.clone(),
        }
    }

    /// When it goes away on its own: the model's summary stays until you
    /// type; facts alone go a while after they appeared, or after the
    /// summary didn't come.
    pub(super) fn until(&self) -> Option<Instant> {
        match (self.summary(), llm::enabled()) {
            (Some(Some(_)), _) => None,
            (Some(None), _) => Some(self.summary_at.unwrap_or(self.shown).max(self.shown) + RECAP_SHOWN),
            (None, true) => Some(self.shown + RECAP_WAIT),
            (None, false) => Some(self.shown + RECAP_SHOWN),
        }
    }
}

impl Pane {
    /// What the coding agent running here is doing, if one is.
    pub fn agent(&mut self, now: Instant) -> Option<AgentState> {
        let running = self.session.active().filter(|block| block.kind == BlockKind::Command);
        let is_agent = running.is_some_and(|block| {
            self.agent_block == Some(block.id)
                || agent::is_agent_command(&block.command)
                || agent::is_agent_title(&self.program_title)
        });
        let block = running.filter(|_| is_agent).map(|block| block.id);
        if block != self.agent_block {
            self.agent_program = None;
        }
        self.agent_block = block;
        if !is_agent {
            self.agent_was = None;
            self.agent_unseen = false;
            self.header_open = false;
            self.screen = None;
            self.watcher = None;
            self.typed.clear();
            self.typed_prompt = None;
            self.turn = None;
            self.last_turn = None;
            return None;
        }
        if self.agent_program.is_none() {
            self.agent_program = running
                .and_then(|block| agent::agent_program(&block.command))
                .or_else(|| agent::title_program(&self.program_title));
        }
        if let Some(group) = self.session.foreground_group() {
            self.watcher.get_or_insert_with(agent::Watcher::new).set_group(group);
        }
        if self.screen.is_none_or(|(at, _)| now.duration_since(at) >= AGENT_SCAN) {
            self.screen = Some((now, self.screen_tail().as_deref().and_then(agent::screen_state)));
        }
        let screen = self.screen.and_then(|(_, state)| state);
        let title = agent::title_state(&self.program_title);
        // Claude Code's own record of whether it's mid-turn, for when a
        // long tool call leaves the screen and title quiet.
        let busy = self.watcher.as_ref().and_then(agent::Watcher::report).is_some_and(|report| report.busy);
        let input = self.session.input_at();
        let notified = self.notified_at.is_some_and(|at| input.is_none_or(|input| at > input));
        // Output that isn't just the echo of your typing. On its own it only
        // keeps a turn going or starts one you just asked for (with Enter):
        // an agent at rest repaints too (status line, a resize, your typing,
        // a scroll or click in it), and that isn't a turn to pop green for.
        let in_turn = matches!(self.agent_was, Some(AgentState::Running | AgentState::Waiting));
        let asked = self.session.submitted_at().is_some_and(|at| now.duration_since(at) < AGENT_ASKED);
        let printing = (in_turn || asked)
            && self.session.output_at.is_some_and(|at| {
                now.duration_since(at) < AGENT_QUIET && input.is_none_or(|input| at.duration_since(input) > AGENT_ECHO)
            });
        let state = if notified
            || title == Some(AgentState::Waiting)
            || (screen == Some(AgentState::Waiting) && title != Some(AgentState::Running))
        {
            AgentState::Waiting
        } else if title == Some(AgentState::Running)
            || self.progress
            || screen == Some(AgentState::Running)
            || printing
            || busy
        {
            AgentState::Running
        } else {
            AgentState::Idle
        };
        match state {
            AgentState::Idle
                if self.agent_was == Some(AgentState::Running)
                    && self.turn.is_some_and(|start| now.duration_since(start) >= AGENT_MIN_TURN) =>
            {
                self.agent_unseen = true
            }
            AgentState::Idle => {}
            _ => self.agent_unseen = false,
        }
        if input.is_some_and(|input| now.duration_since(input) < AGENT_ECHO) {
            self.agent_unseen = false;
        }
        // A turn runs from when it starts working until it's back at rest;
        // stopping to ask you something is part of it.
        if state == AgentState::Idle {
            if let Some(start) = self.turn.take() {
                self.last_turn = Some(now.duration_since(start));
            }
        } else if self.turn.is_none() {
            self.turn = Some(now);
        }
        self.agent_was = Some(state);
        Some(if state == AgentState::Idle && self.agent_unseen { AgentState::Done } else { state })
    }
    /// What the header over the agent here shows, as of the last `agent()`.
    pub fn agent_info(&self, now: Instant) -> Option<AgentInfo> {
        let state = self.agent_now?;
        let report = self.watcher.as_ref().and_then(agent::Watcher::report).unwrap_or_default();
        let started = self.session.active().map_or(now, |block| block.started);
        Some(AgentInfo {
            program: self.agent_program,
            state,
            prompt: report.prompt.or_else(|| self.typed_prompt.clone()),
            subagents: report.subagents,
            subagent_tasks: report.subagent_tasks,
            queued: report.queued,
            queue: report.queue,
            name: report.name,
            turn: self.turn.map(|start| now.duration_since(start)).or(self.last_turn),
            uptime: now.duration_since(started),
            recap: self.recap.as_ref().map(Recap::line),
            cwd: self.session.cwd.clone(),
            branch: self.session.branch.clone(),
            open: self.header_open,
        })
    }
    pub fn focus_lost(&mut self, now: Instant) {
        self.left_at = Some(now);
    }
    /// Coming back to the pane: after `idle` or longer since the user last
    /// did anything in it (output doesn't count: an agent works while
    /// they're away), or when asked (`idle` None), a recap shows. `mark`
    /// is what the tab's mark adds to it.
    pub fn focus_gained(&mut self, now: Instant, idle: Option<Duration>, mark: Option<MarkNote>) {
        let last = [self.left_at, self.session.input_at()].into_iter().flatten().max();
        let left = self.left_at.take();
        let away = last.map_or(Duration::ZERO, |last| now.duration_since(last));
        let due = match idle {
            None => true,
            Some(idle) => left.is_some() && !idle.is_zero() && away >= idle,
        };
        if due {
            self.show_recap(now, away, mark);
        }
    }
    pub(super) fn show_recap(&mut self, now: Instant, away: Duration, mark: Option<MarkNote>) {
        let mut facts = Vec::new();
        if away >= Duration::from_secs(60) {
            facts.push(format!("↩ away {}", ui::short_duration(away)));
        } else {
            facts.push("↩".to_owned());
        }
        if let Some(mark) = &mark {
            facts.push(mark.fact.clone());
        }
        let finished =
            self.session.blocks.iter().rev().find(|block| block.kind == BlockKind::Command && !block.running());
        match (self.session.active(), finished) {
            (Some(block), _) => facts.push(format!("running: {}", agent::one_line(&block.command))),
            (None, Some(block)) => {
                let status = match block.exit {
                    Some(0) | None => "✓".to_owned(),
                    Some(code) => format!("✗ exit {code}"),
                };
                let ago = block.finished.map(|at| format!(" · {} ago", ui::short_duration(now.duration_since(at))));
                let command = agent::one_line(&block.command);
                let command = if command.is_empty() { "command".to_owned() } else { command };
                facts.push(format!("last: {command} {status}{}", ago.unwrap_or_default()));
            }
            (None, None) => {}
        }
        let place = ui::short_path(&self.session.cwd);
        facts.push(match &self.session.branch {
            Some(branch) => format!("{place} ({branch})"),
            None => place,
        });
        if let Some(info) = self.agent_info(now) {
            facts.push(format!("agent {}", info.state.label().to_lowercase()));
        }
        let summary = Arc::new(Mutex::new(None));
        if llm::enabled() {
            let mut context = self.recap_context(now);
            if let Some(mark) = &mark {
                context = format!("{}\n{context}", mark.context);
            }
            let theirs = summary.clone();
            let _ = std::thread::Builder::new().name("recap".into()).spawn(move || {
                let answer = llm::complete(RECAP_SYSTEM, &context, 120).ok().filter(|answer| !answer.is_empty());
                *theirs.lock().unwrap() = Some(answer.map(|answer| agent::one_line(&answer)));
            });
        } else {
            *summary.lock().unwrap() = Some(None);
        }
        self.recap = Some(Recap { shown: now, facts: facts.join(" · "), summary, summary_at: None });
    }
    /// What the summary is written from: the folder, the agent's last
    /// prompt, and the last few commands with how they ended and the end
    /// of their output, newest last.
    pub fn recap_context(&mut self, now: Instant) -> String {
        let mut parts = vec![format!("Folder: {}", self.session.cwd)];
        if let Some(branch) = &self.session.branch {
            parts.push(format!("Git branch: {branch}"));
        }
        if let Some(info) = self.agent_info(now) {
            parts.push(format!("A coding agent runs here, {}.", info.state.label().to_lowercase()));
            if let Some(prompt) = info.prompt {
                parts.push(format!("Last prompt to the agent: {prompt}"));
            }
        }
        let first = self.session.blocks.len().saturating_sub(RECAP_BLOCKS);
        let mut blocks = Vec::new();
        for block in self.session.blocks[first..].iter_mut().filter(|block| block.kind == BlockKind::Command) {
            let status = match (block.running(), block.exit) {
                (true, _) => "still running".to_owned(),
                (false, Some(code)) => format!("exit {code}"),
                (false, None) => "finished".to_owned(),
            };
            let lines = block.output_lines();
            let output = if lines == 0 {
                String::new()
            } else {
                block.text_between((lines.saturating_sub(RECAP_LINES), 0), (lines - 1, usize::MAX))
            };
            let output: String =
                output.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
            blocks.push(format!("$ {} ({status})\n{output}", block.command.trim()));
        }
        parts.push(blocks.join("\n\n"));
        let context = parts.join("\n");
        // Keep the end: it's the most recent.
        let skip = context.chars().count().saturating_sub(RECAP_CHARS);
        context.chars().skip(skip).collect()
    }
    /// One line on what the pane is doing: the running command, or the
    /// folder at rest; and whether something runs.
    pub fn activity(&self) -> (bool, String) {
        let running = self.session.active().map(|block| agent::one_line(&block.command));
        match running {
            Some(command) if !command.is_empty() => (true, command),
            Some(_) if !self.program_title.is_empty() => (true, agent::one_line(&self.program_title)),
            running => (running.is_some(), ui::short_path(&self.session.cwd)),
        }
    }
    /// A line on an idle shell for the extended sidebar: its folder when
    /// not `base` (the tab's), the last command, how it ended and how long
    /// ago, and the git branch with its count of changed files.
    pub fn idle_line(&self, base: &str, now: Instant) -> String {
        let mut parts = Vec::new();
        let cwd = &self.session.cwd;
        if cwd != base {
            parts.push(match cwd.strip_prefix(base).filter(|rest| rest.starts_with('/')) {
                Some(rest) => format!(".{rest}"),
                None => ui::short_path(cwd),
            });
        }
        match self.session.last_finished() {
            Some(block) => {
                let mark = if block.failed() { "✗" } else { "✓" };
                let mut last = format!("{mark} {}", agent::one_line(&block.command));
                if let Some(code) = block.exit.filter(|&code| code != 0) {
                    last.push_str(&format!(" (exit {code})"));
                }
                if let Some(at) = block.finished {
                    last.push_str(&format!(" · {}", ago(now.saturating_duration_since(at))));
                }
                parts.push(last);
            }
            None if parts.is_empty() => parts.push(format!("shell · {}", self.session.shell_name)),
            None => {}
        }
        if let Some(branch) = &self.session.branch {
            match self.git_changes.as_ref().filter(|(dir, _)| dir == cwd) {
                Some((_, n)) if *n > 0 => parts.push(format!("{branch} ●{n}")),
                _ => parts.push(branch.clone()),
            }
        }
        parts.join(" · ")
    }
    /// Whether the last command failed, for the sidebar's red dot.
    pub fn last_failed(&self) -> bool {
        self.session.last_finished().is_some_and(Block::failed)
    }
    /// Asks git, off the main thread, how many files changed in this idle
    /// pane's repository: again after each command, a new folder, or a while.
    pub fn refresh_git(&mut self, sender: &Sender<Msg>, now: Instant) {
        if self.session.branch.is_none() || self.session.active().is_some() {
            return;
        }
        let cwd = self.session.cwd.clone();
        let last = self.session.last_finished().map(|block| block.id);
        let fresh = |(dir, block, at): &(String, Option<u64>, Instant)| {
            *dir == cwd && *block == last && now.saturating_duration_since(*at) < GIT_RECHECK
        };
        if self.git_asked.as_ref().is_some_and(fresh) {
            return;
        }
        self.git_asked = Some((cwd.clone(), last, now));
        let (id, sender, path) = (self.id, sender.clone(), self.session.path.clone());
        std::thread::spawn(move || {
            let mut git = std::process::Command::new("git");
            git.args(["-C", &cwd, "status", "--porcelain", "--untracked-files=normal"]);
            if let Some(path) = path {
                git.env("PATH", path);
            }
            let Ok(output) = git.stderr(std::process::Stdio::null()).output() else { return };
            if output.status.success() {
                let changed = output.stdout.split(|&b| b == b'\n').filter(|line| !line.is_empty()).count();
                let _ = sender.send(Msg::Git(id, cwd, changed));
            }
        });
    }
    /// Follows what's typed to an agent, to know its prompt when it keeps
    /// no records we read. Answers to its questions aren't prompts.
    pub(super) fn track_typing(&mut self, text: &str) {
        if self.agent_block.is_none() || self.agent_was == Some(AgentState::Waiting) {
            self.typed.clear();
            return;
        }
        for c in text.chars() {
            match c {
                '\r' => {
                    let line = agent::one_line(&self.typed);
                    if !line.is_empty() {
                        self.typed_prompt = Some(line);
                    }
                    self.typed.clear();
                }
                '\u{7f}' | '\u{8}' => {
                    self.typed.pop();
                }
                // Control+C or Control+U: the line is gone.
                '\u{3}' | '\u{15}' => self.typed.clear(),
                // Arrows and the like: we can't follow the line any more.
                '\u{1b}' => return,
                c if c.is_control() => {}
                c => self.typed.push(c),
            }
        }
    }
    /// The last few lines the running program left on screen.
    pub(super) fn screen_tail(&mut self) -> Option<String> {
        let block = self.session.active_mut()?;
        let lines = block.output_lines();
        if lines == 0 {
            return None;
        }
        Some(block.text_between((lines.saturating_sub(AGENT_SCAN_LINES), 0), (lines - 1, usize::MAX)))
    }
    /// What a remote shows of the pane, a screenful of lines: a full-screen
    /// program's screen as it is, or else the last commands with their
    /// output, then the prompt with what's typed at it.
    /// What a remote client draws for this pane, as JSON: a full-screen
    /// program's grid with its cursor, or the last blocks plus the editor.
    /// Lines go into `lines` by key (`prefix` plus where they are) and
    /// stand in the JSON as that key, so a client gets each line once.
    pub fn remote_screen(&mut self, ctx: &mut Ctx, prefix: &str, lines_out: &mut HashMap<String, String>) -> String {
        use crate::remote::{quote, LINE};
        let mut line_ref = |key: String, json: String| {
            let marker = format!("{LINE}{key}{LINE}");
            lines_out.insert(key, json);
            marker
        };
        let suggestion = if self.session.editor_active() { self.suggestion(ctx) } else { None };
        // The command line in the Mac's syntax colors (a known command green…).
        let editor_runs = if self.session.editor_active() {
            let text = &self.editor.text;
            let mut colors = vec![ctx.theme.text; text.len()];
            if !self.ai {
                ctx.commands.use_path(self.session.path.as_deref());
                for (start, end, color) in crate::ui::highlight(text, self.editor.cursor, &mut ctx.commands, &ctx.theme)
                {
                    for slot in &mut colors[start.min(text.len())..end.min(text.len())] {
                        *slot = color;
                    }
                }
            }
            let mut runs: Vec<(String, String)> = Vec::new();
            for (offset, c) in text.char_indices() {
                let color = colors[offset].css();
                match runs.last_mut() {
                    Some(run) if run.1 == color => run.0.push(c),
                    _ => runs.push((c.to_string(), color)),
                }
            }
            let runs: Vec<String> =
                runs.iter().map(|(t, color)| format!(r#"[{},"{color}",null,0]"#, quote(t))).collect();
            format!("[{}]", runs.join(","))
        } else {
            "[]".to_owned()
        };
        let theme = &ctx.theme;
        let rows = self.session.size.lines.max(1);
        let columns = self.session.size.columns;
        if let Some(block) = self.session.active_mut().filter(|block| block.alt_screen()) {
            let top = block.term.grid().history_size();
            let lines: Vec<String> = (top..top + rows)
                .map(|i| line_ref(format!("{prefix}g{}", i - top), crate::ui::remote_row(block, i, theme)))
                .collect();
            let cursor = if block.term.mode().contains(alacritty_terminal::term::TermMode::SHOW_CURSOR) {
                let point = block.term.grid().cursor.point;
                format!("[{},{}]", point.line.0, point.column.0)
            } else {
                "null".to_owned()
            };
            return format!(r#"{{"mode":"grid","columns":{columns},"lines":[{}],"cursor":{cursor}}}"#, lines.join(","));
        }
        // Each block's last screenful at most; the client scrolls.
        let start = self.session.blocks.len().saturating_sub(12);
        let mut blocks = Vec::new();
        let live = self.session.phase == crate::session::Phase::Running;
        let count = self.session.blocks.len();
        for (i, block) in self.session.blocks[start..].iter_mut().enumerate() {
            let n = block.output_lines();
            let id = block.id;
            let lines: Vec<String> = (n.saturating_sub(rows * 2)..n)
                .map(|line| line_ref(format!("{prefix}{id}.{line}"), crate::ui::remote_row(block, line, theme)))
                .collect();
            let running = live && start + i + 1 == count && block.running();
            // A running block's cursor, for programs that read a line.
            let cursor = if running {
                let grid = block.term.grid();
                let point = grid.cursor.point;
                let index = (grid.history_size() as i32 + point.line.0) as usize;
                let first = n.saturating_sub(rows * 2);
                if index >= first && index < n.max(index + 1) {
                    format!("[{},{}]", index - first, point.column.0)
                } else {
                    "null".to_owned()
                }
            } else {
                "null".to_owned()
            };
            blocks.push(format!(
                r#"{{"id":{},"kind":{},"command":{},"cwd":{},"branch":{},"exit":{},"running":{running},"secs":{:.1},"lines":[{}],"cursor":{cursor}}}"#,
                block.id,
                quote(match block.kind {
                    BlockKind::Command => "command",
                    BlockKind::Ai => "ai",
                    BlockKind::Background => "output",
                }),
                quote(&block.command),
                quote(&block.cwd),
                block.branch.as_deref().map_or("null".to_owned(), quote),
                block.exit.map_or("null".to_owned(), |code| code.to_string()),
                block.duration().as_secs_f32(),
                lines.join(","),
            ));
        }
        let editor = if self.session.editor_active() {
            let cursor = self.editor.text[..self.editor.cursor.min(self.editor.text.len())].chars().count();
            let menu = self.completion.as_ref().map_or("null".to_owned(), |c| {
                let items: Vec<String> = c
                    .candidates
                    .iter()
                    .take(50)
                    .map(|candidate| {
                        let kind = match candidate.kind {
                            crate::complete::Kind::Command => "command",
                            crate::complete::Kind::Directory => "directory",
                            crate::complete::Kind::File => "file",
                            crate::complete::Kind::History => "history",
                        };
                        format!(r#"[{},"{kind}"]"#, quote(&candidate.label))
                    })
                    .collect();
                format!(r#"{{"items":[{}],"selected":{}}}"#, items.join(","), c.selected)
            });
            format!(
                r#"{{"text":{},"runs":{editor_runs},"cursor":{cursor},"cwd":{},"branch":{},"suggestion":{},"ai":{},"menu":{menu}}}"#,
                quote(&self.editor.text),
                quote(&self.session.cwd),
                self.session.branch.as_deref().map_or("null".to_owned(), quote),
                suggestion.as_deref().map_or("null".to_owned(), quote),
                self.ai,
            )
        } else {
            "null".to_owned()
        };
        format!(r#"{{"mode":"blocks","columns":{columns},"blocks":[{}],"editor":{editor}}}"#, blocks.join(","))
    }
}

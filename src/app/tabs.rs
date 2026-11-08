use super::*;

/// What a tab is waiting on you for, set by hand so a glance at the list
/// says what to do with it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Mark {
    /// Deal with it first.
    Important,
    /// Done for now, held up by something else.
    Waiting,
    /// Set aside to come back to.
    Later,
}

impl Mark {
    pub(super) const ALL: [Mark; 3] = [Mark::Important, Mark::Waiting, Mark::Later];

    pub(super) fn label(self) -> &'static str {
        match self {
            Mark::Important => "Important",
            Mark::Waiting => "Waiting",
            Mark::Later => "Later",
        }
    }

    pub(super) fn color(self, theme: &Theme) -> Color {
        match self {
            Mark::Important => theme.error,
            Mark::Waiting => theme.warning,
            Mark::Later => theme.text_faint,
        }
    }

    /// What it means, for the model writing a recap.
    pub(super) fn meaning(self) -> &'static str {
        match self {
            Mark::Important => "important, to deal with first",
            Mark::Waiting => "waiting: done for now and held up by something else",
            Mark::Later => "set aside to come back to later",
        }
    }

    /// Set aside: ⌘` passes it by, and typing in it clears the mark.
    pub(super) fn aside(self) -> bool {
        self != Mark::Important
    }

    /// How it's written in the saved tabs file.
    pub(super) fn code(self) -> &'static str {
        match self {
            Mark::Important => "important",
            Mark::Waiting => "waiting",
            Mark::Later => "later",
        }
    }

    pub(super) fn from_code(code: &str) -> Option<Mark> {
        Mark::ALL.into_iter().find(|mark| mark.code() == code)
    }
}

impl App {
    pub(super) fn close_tab(&mut self, index: usize) {
        let mut leaves = Vec::new();
        self.tabs[index].root.leaves(&mut leaves);
        for id in leaves {
            self.close_pane(id);
        }
    }
    pub(super) fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let before = self.focused_id();
        self.active = index;
        self.relayout();
        self.focus_moved(before);
    }
    pub(super) fn toggle_tab_list(&mut self) {
        let visible = !self.tab_list_visible();
        self.tab_list = Some(visible);
        self.relayout();
        if !visible {
            self.ctx.show_toast(&format!("Tabs hidden · {}+B shows them", platform::COMMAND_NAME));
        }
    }
    pub(super) fn step_tab(&mut self, delta: i32) {
        let n = self.tabs.len() as i32;
        if n > 1 {
            self.switch_tab((self.active as i32 + delta).rem_euclid(n) as usize);
        }
    }
    /// Moves tab `from` to position `to` in the list; the active tab stays active.
    /// Sets tab `index`'s mark to `mark`, or clears it when it's already set.
    pub(super) fn toggle_mark(&mut self, index: usize, mark: Mark) {
        let Some(tab) = self.tabs.get_mut(index) else { return };
        tab.mark = if tab.mark == Some(mark) { None } else { Some(mark) };
        tab.marked_at = tab.mark.map(|_| Instant::now());
        let toast = match tab.mark {
            Some(mark) => format!("Marked {}", mark.label()),
            None => "Mark cleared".to_owned(),
        };
        self.ctx.show_toast(&toast);
        self.save_tabs();
        self.dirty = true;
    }
    /// The tab holding pane `id`.
    pub(super) fn tab_of(&self, id: u64) -> Option<&Tab> {
        self.tabs.iter().find(|tab| {
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            leaves.contains(&id)
        })
    }
    /// The mark on the tab holding pane `id`.
    pub(super) fn mark_of(&self, id: u64) -> Option<Mark> {
        self.tab_of(id).and_then(|tab| tab.mark)
    }
    /// What the mark on pane `id`'s tab adds to its recap.
    pub(super) fn mark_note(&self, id: u64, now: Instant) -> Option<MarkNote> {
        let tab = self.tab_of(id)?;
        let mark = tab.mark?;
        let ago = tab.marked_at.map(|at| ui::short_duration(now.duration_since(at)));
        Some(MarkNote {
            fact: match &ago {
                Some(ago) => format!("{} · marked {ago} ago", mark.label()),
                None => mark.label().to_owned(),
            },
            context: match &ago {
                Some(ago) => format!("The user marked this tab {} {ago} ago.", mark.meaning()),
                None => format!("The user marked this tab {}.", mark.meaning()),
            },
        })
    }
    pub(super) fn move_tab(&mut self, from: usize, to: usize) {
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        // Every tab between the two ends moved; each slides over from where
        // it shows now.
        let now = Instant::now();
        for tab in &mut self.tabs[from.min(to)..=from.max(to)] {
            tab.slide = tab.shown.map(|at| (at, now));
        }
        self.active = if self.active == from {
            to
        } else if from < self.active && self.active <= to {
            self.active - 1
        } else if to <= self.active && self.active < from {
            self.active + 1
        } else {
            self.active
        };
    }
    pub(super) fn start_rename(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else { return };
        let name = tab.name.clone().unwrap_or_else(|| self.tab_title(index));
        self.renaming = Some(Rename { tab: index, name, selected: true });
    }
    /// Ends renaming, keeping the name typed; an empty name goes back to the
    /// pane's title.
    pub(super) fn finish_rename(&mut self) {
        let Some(rename) = self.renaming.take() else { return };
        if let Some(tab) = self.tabs.get_mut(rename.tab) {
            let name = rename.name.trim().to_owned();
            // Left as it was, the title keeps following the panes.
            let typed = !name.is_empty() && tab.name.as_deref() != Some(&name);
            if typed || name.is_empty() {
                let auto = self.auto_title(rename.tab);
                let tab = &mut self.tabs[rename.tab];
                tab.name = (typed && name != auto).then_some(name);
            }
        }
        self.save_tabs();
    }
    /// A key while renaming a tab; false lets it through.
    pub(super) fn rename_key(&mut self, key: &KeyEvent) -> bool {
        let Some(rename) = self.renaming.as_mut() else { return false };
        if key.cmd() {
            if key.base_char() == Some('a') {
                rename.selected = true;
                return true;
            }
            self.finish_rename();
            return false;
        }
        match key.named {
            Some(Named::Enter) => self.finish_rename(),
            Some(Named::Escape) => self.renaming = None,
            Some(Named::Backspace | Named::Delete) if rename.selected => rename.name.clear(),
            Some(Named::Backspace) => {
                rename.name.pop();
            }
            Some(_) => {}
            None if !key.ctrl() => {
                let typed: String = String::from_utf8_lossy(&key.bytes).chars().filter(|c| !c.is_control()).collect();
                if typed.is_empty() {
                    return true;
                }
                if rename.selected {
                    rename.name.clear();
                }
                rename.name.push_str(&typed);
            }
            None => {}
        }
        if let Some(rename) = self.renaming.as_mut() {
            rename.selected = false;
        }
        true
    }
    pub(super) fn open_tab_menu(&mut self, tab: usize, x: f32, y: f32) {
        self.finish_rename();
        self.switch_tab(tab);
        self.tab_menu = Some(TabMenu { tab, x, y, kind: MenuKind::Tab });
    }
    pub(super) fn menu_action(&mut self, tab: usize, action: MenuAction) {
        self.tab_menu = None;
        match action {
            MenuAction::Rename => self.start_rename(tab),
            MenuAction::Color(color) => {
                if let Some(tab) = self.tabs.get_mut(tab) {
                    tab.color = color;
                }
                self.save_tabs();
            }
            MenuAction::Mark(mark) => self.toggle_mark(tab, mark),
            MenuAction::Close => self.close_tab(tab),
            MenuAction::CloseOthers => {
                for index in (0..self.tabs.len()).rev().filter(|&i| i != tab) {
                    self.close_tab(index);
                }
            }
            MenuAction::SplitRight => self.split(Axis::Row, false),
            MenuAction::SplitDown => self.split(Axis::Column, false),
            MenuAction::ClosePane => {
                if let Some(id) = self.focused_id() {
                    self.close_pane(id);
                }
            }
            MenuAction::Settings => self.open_settings(),
            MenuAction::Account => {
                self.open_settings();
                self.settings.to_remote = true;
                self.dirty = true;
            }
            #[cfg(not(target_os = "haiku"))]
            MenuAction::SignIn => crate::relay::sign_in(),
            #[cfg(not(target_os = "haiku"))]
            MenuAction::SignOut => {
                crate::relay::sign_out();
                self.dirty = true;
            }
            #[cfg(target_os = "haiku")]
            MenuAction::SignIn | MenuAction::SignOut => {}
            MenuAction::Copy | MenuAction::Paste | MenuAction::Clear => {
                let Some(id) = self.focused_id() else { return };
                let Self { panes, ctx, .. } = self;
                let Some(pane) = panes.iter_mut().find(|p| p.id == id) else { return };
                match action {
                    MenuAction::Copy => pane.copy_selection(ctx),
                    MenuAction::Paste => pane.paste_clipboard(),
                    _ => pane.clear(),
                }
            }
        }
    }
    /// A tab's name, or its focused pane's title.
    pub(super) fn tab_title(&self, index: usize) -> String {
        let tab = &self.tabs[index];
        if tab.focus == SETTINGS {
            return "Settings".to_owned();
        }
        tab.name
            .clone()
            .or_else(|| self.viewers.iter().find(|v| v.id == tab.focus).map(|v| v.title.clone()))
            .or_else(|| tab.ai_title.title.clone())
            .unwrap_or_else(|| self.auto_title(index))
    }
    /// What the model titles tab `index` from: each terminal pane's folder
    /// and branch, its agent's task, and what it has been running.
    pub(super) fn title_context(&self, index: usize, now: Instant) -> String {
        let mut leaves = Vec::new();
        self.tabs[index].root.leaves(&mut leaves);
        let mut parts = Vec::new();
        for pane in leaves.iter().filter_map(|id| self.panes.iter().find(|p| p.id == *id)) {
            let mut part = format!("Pane in {}", ui::short_path(&pane.session.cwd));
            if let Some(branch) = &pane.session.branch {
                part += &format!(" (git branch {branch})");
            }
            if let Some(info) = pane.agent_info(now) {
                let name = info.program.map_or("A coding agent", |p| agent::agent_brand(p).0);
                match &info.prompt {
                    Some(prompt) => part += &format!(". {name} was asked: {}", agent::one_line(prompt)),
                    None => part += &format!(". {name} runs here"),
                }
            } else {
                let recent = pane.recent_commands();
                if !recent.is_empty() {
                    part += &format!(". Recent commands: {}", recent.join(", "));
                }
            }
            parts.push(part);
        }
        parts.join("\n")
    }
    /// What the model writes a tab's title and summary from: the summary
    /// before, then each pane's folder, agent and prompt, and its last few
    /// commands with the end of their output.
    pub(super) fn summary_context(&mut self, index: usize, now: Instant) -> String {
        let mut leaves = Vec::new();
        self.tabs[index].root.leaves(&mut leaves);
        let mut parts = Vec::new();
        if let Some(before) = &self.tabs[index].ai_title.summary {
            parts.push(format!("Before, the summary was: {before}"));
        }
        let panes: Vec<&mut Pane> = self.panes.iter_mut().filter(|p| leaves.contains(&p.id)).collect();
        let each = TITLE_CHARS / panes.len().max(1);
        for (i, pane) in panes.into_iter().enumerate() {
            let context = pane.recap_context(now);
            // Keep the end: it's the most recent.
            let skip = context.chars().count().saturating_sub(each);
            parts.push(format!("Pane {}:\n{}", i + 1, context.chars().skip(skip).collect::<String>()));
        }
        parts.join("\n\n")
    }
    /// Keeps tabs' AI titles and summaries current: takes in answers, and
    /// asks again for a tab whose panes have settled on something new.
    pub(super) fn refresh_titles(&mut self, now: Instant) {
        if !crate::llm::enabled() {
            return;
        }
        let mut wake: Option<Instant> = None;
        for index in 0..self.tabs.len() {
            let tab = &self.tabs[index];
            if tab.focus == SETTINGS || self.viewers.iter().any(|v| v.id == tab.focus) {
                continue;
            }
            let context = self.title_context(index, now);
            let title = &mut self.tabs[index].ai_title;
            if let Some(answer) = &title.answer {
                let Some(answer) = answer.lock().unwrap().clone() else {
                    wake = Some(now + Duration::from_millis(250));
                    continue;
                };
                title.answer = None;
                if let Some((name, summary)) = answer {
                    title.title = Some(name);
                    title.summary = summary.or(title.summary.take());
                    self.dirty = true;
                }
            }
            if context.is_empty() || context == title.asked_for {
                continue;
            }
            if context != title.seen {
                title.seen = context.clone();
                title.seen_at = Some(now);
            }
            let due = [title.seen_at.map(|at| at + TITLE_SETTLE), title.asked_at.map(|at| at + TITLE_EVERY)]
                .into_iter()
                .flatten()
                .max()
                .unwrap_or(now);
            if now < due {
                wake = Some(wake.map_or(due, |w| w.min(due)));
                continue;
            }
            title.asked_for = context;
            title.asked_at = Some(now);
            let answer = Arc::new(std::sync::Mutex::new(None));
            title.answer = Some(answer.clone());
            let prompt = self.summary_context(index, now);
            let _ = std::thread::Builder::new().name("tab title".into()).spawn(move || {
                let reply = crate::llm::complete(TITLE_SYSTEM, &prompt, 90).ok();
                *answer.lock().unwrap() = Some(reply.as_deref().and_then(title_and_summary));
            });
            wake = Some(wake.map_or(now + Duration::from_millis(250), |w| w.min(now + Duration::from_millis(250))));
        }
        if let Some(at) = wake {
            self.animation_at = Some(self.animation_at.map_or(at, |a| a.min(at)));
        }
    }
    /// A tab's title when it isn't named: its panes' folders, in layout
    /// order and each once, then what the focused pane has been running.
    pub(super) fn auto_title(&self, index: usize) -> String {
        let tab = &self.tabs[index];
        let mut leaves = Vec::new();
        tab.root.leaves(&mut leaves);
        let mut folders: Vec<String> = Vec::new();
        for pane in leaves.iter().filter_map(|id| self.panes.iter().find(|p| p.id == *id)) {
            let folder = pane.folder();
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        let recent = self.panes.iter().find(|p| p.id == tab.focus).map(Pane::recent_commands).unwrap_or_default();
        let folders = folders.join(" + ");
        if recent.is_empty() {
            folders
        } else {
            format!("{folders} · {}", recent.join(", "))
        }
    }
    /// Reads what every pane's agent is doing, once a frame; a pane whose
    /// agent came or went makes room for its header, or gives it back.
    /// Returns the most urgent state in each tab.
    pub(super) fn refresh_agents(&mut self) -> Vec<Option<AgentState>> {
        let now = Instant::now();
        let mut moved = false;
        // Away from the Mac (no Temino window in front): phones get pinged.
        let away = !self.focused;
        let agents = (0..self.tabs.len())
            .map(|index| {
                let mut leaves = Vec::new();
                self.tabs[index].root.leaves(&mut leaves);
                let seen = index == self.active && self.focused;
                let panes = self.panes.iter_mut().filter(|pane| leaves.contains(&pane.id));
                panes
                    .filter_map(|pane| {
                        if seen {
                            pane.agent_unseen = false;
                        }
                        let state = pane.agent(now);
                        moved |= state.is_some() != pane.agent_now.is_some();
                        // Newly waiting or done while you're away from the
                        // Mac: the phones hear of it.
                        if let Some(need @ (AgentState::Waiting | AgentState::Done)) = state {
                            if away && pane.needs_since.map(|(was, _)| was) != Some(need) {
                                #[cfg(not(target_os = "haiku"))]
                                crate::relay::notify(
                                    if need == AgentState::Waiting { "waiting" } else { "done" },
                                    index + 1,
                                );
                            }
                        }
                        pane.agent_now = state;
                        pane.needs_since = match state {
                            Some(need @ (AgentState::Waiting | AgentState::Done)) => match pane.needs_since {
                                Some((was, at)) if was == need => Some((was, at)),
                                _ => Some((need, now)),
                            },
                            _ => None,
                        };
                        let program = pane.header_program();
                        moved |= program.is_some() != pane.program_now.is_some();
                        pane.program_now = program;
                        state
                    })
                    .max()
            })
            .collect();
        if moved {
            self.relayout();
        }
        agents
    }
    /// A working agent's ring turns; any agent is read again soon, and its
    /// header's clock ticks.
    pub(super) fn wake_for_agents(&mut self, agents: &[Option<AgentState>]) {
        let now = Instant::now();
        let wake = match agents.iter().flatten().max() {
            // Smooth frames while something animates: a turning ring, an
            // amber pulse, a green pop and its heartbeat.
            Some(AgentState::Running | AgentState::Waiting | AgentState::Done) => Some(now + Duration::from_millis(33)),
            Some(_) => Some(now + Duration::from_millis(400)),
            None => None,
        };
        if let Some(at) = wake {
            self.animation_at = Some(self.animation_at.map_or(at, |a| a.min(at)));
        }
    }
}

pub(super) fn recap_idle() -> Duration {
    let minutes = std::env::var("TEMINO_RECAP_MINUTES")
        .ok()
        .or_else(|| crate::setting("recap_minutes"))
        .and_then(|minutes| minutes.parse::<f32>().ok())
        .unwrap_or(10.0);
    Duration::from_secs_f32(minutes.max(0.0) * 60.0)
}

/// The model's two lines: a tab's title and its summary. Labels it may
/// add anyway ("Title:", "2.") come off; a title too long isn't one.
pub(super) fn title_and_summary(reply: &str) -> Option<(String, Option<String>)> {
    let mut lines = reply.lines().map(str::trim).filter(|line| !line.is_empty()).map(|line| {
        let line = line.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | ')' | '-' | '*' | ' '));
        let line = ["Title:", "Summary:", "Line 1:", "Line 2:"]
            .iter()
            .find_map(|label| line.strip_prefix(label))
            .unwrap_or(line);
        line.trim().trim_matches(['"', '*', ' '])
    });
    let title = lines.next()?.trim_end_matches('.').to_owned();
    if title.is_empty() || title.chars().count() > 60 {
        return None;
    }
    let summary = lines.next().filter(|s| !s.is_empty() && s.chars().count() <= 200).map(str::to_owned);
    Some((title, summary))
}

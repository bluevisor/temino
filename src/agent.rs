//! Spotting a coding agent (Claude Code, Codex, Gemini, ...) in a pane and
//! reading what it's up to from its command, its window title and what it
//! leaves on screen, and, for Claude Code, from the session records and
//! transcripts it keeps under ~/.claude.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::json::{self, Value};

/// What an agent in a pane is doing, least to most urgent.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum AgentState {
    /// At its prompt, nothing new.
    Idle,
    /// Finished a turn you haven't looked at yet.
    Done,
    /// Working.
    Running,
    /// Asking you something: a permission, a question, a notification.
    Waiting,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            AgentState::Idle => "Idle",
            AgentState::Done => "Done",
            AgentState::Running => "Running",
            AgentState::Waiting => "Waiting for you",
        }
    }
}

/// Agent programs, by the name they're run as.
const AGENTS: [&str; 16] = [
    "claude",
    "claude-code",
    "codex",
    "gemini",
    "aider",
    "opencode",
    "cursor-agent",
    // Cursor's CLI, by its newer name.
    "agent",
    "amp",
    "goose",
    "crush",
    "qwen",
    "copilot",
    "droid",
    "kiro-cli",
    "haikucode",
];

/// Words that run the next word as the program.
const LAUNCHERS: [&str; 10] = ["env", "npx", "bunx", "pnpx", "exec", "command", "nohup", "time", "sudo", "uvx"];

/// Other programs that get a header over their pane (their name and how
/// long they've run), by the name they're run as: `htop`, `lazygit`.
const HEADER_PROGRAMS: [&str; 0] = [];

/// The program `command` runs, by name: past launchers and variables,
/// without its path, package scope or version (`FOO=1 npx
/// @anthropic-ai/claude-code@latest` → `claude-code`).
pub fn program_name(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace().filter(|word| !word.starts_with('-'));
    let program = loop {
        match words.next() {
            Some(word) if word.contains('=') || LAUNCHERS.contains(&word) || word == "dlx" => continue,
            Some("pnpm" | "yarn" | "bun" | "npm") => continue,
            Some(word) => break word,
            None => return None,
        }
    };
    let name = program.rsplit('/').next().unwrap_or(program);
    Some(name.split('@').find(|part| !part.is_empty()).unwrap_or(name))
}

/// Whether `command` runs an agent: `claude`, `npx @openai/codex`,
/// `FOO=1 gemini -p ...`.
pub fn is_agent_command(command: &str) -> bool {
    let Some(name) = program_name(command) else { return false };
    let name = name.strip_suffix("-cli").unwrap_or(name);
    AGENTS.contains(&name) || name == "kiro"
}

/// The name to put in the header over `command`'s pane, when it runs one
/// of `HEADER_PROGRAMS`.
pub fn header_program(command: &str) -> Option<&str> {
    program_name(command).filter(|name| HEADER_PROGRAMS.contains(name))
}

/// What an agent's window title says, if it says anything we know.
/// Claude Code titles itself `✳ <topic>` at rest and swaps the ✳ for a
/// turning braille spinner while it works; Gemini CLI says `◇ Ready`,
/// `✦ Working…` and `✋ Action Required`.
pub fn title_state(title: &str) -> Option<AgentState> {
    let first = title.trim_start().chars().next()?;
    match first {
        '\u{2800}'..='\u{28ff}' | '✦' | '⏳' => Some(AgentState::Running),
        '✋' => Some(AgentState::Waiting),
        '✳' | '◇' => Some(AgentState::Idle),
        _ => None,
    }
}

/// Whether a title looks like an agent's own, for agents run under an
/// alias we can't recognize by name.
pub fn is_agent_title(title: &str) -> bool {
    title.trim_start().starts_with('✳') || title.contains("Claude Code") || title.contains("Gemini CLI")
}

/// What the bottom of an agent's screen says: a working line with an
/// interrupt hint, or a question waiting on you.
pub fn screen_state(tail: &str) -> Option<AgentState> {
    let lower = tail.to_lowercase();
    const ASKING: [&str; 13] = [
        "do you want",
        "would you like",
        "allow execution",
        "allow command",
        "apply this change",
        "waiting for your",
        "needs your permission",
        "(y/n)",
        "[y/n]",
        "❯ 1. yes",
        "› 1. yes",
        "▌ yes",
        // Haiku Code: "approve <what>  (Y)es  (N)o".
        "(y)es",
    ];
    const WORKING: [&str; 3] = ["esc to interrupt", "esc to cancel", "ctrl+c to interrupt"];
    if WORKING.iter().any(|hint| lower.contains(hint)) {
        Some(AgentState::Running)
    } else if ASKING.iter().any(|ask| lower.contains(ask)) {
        Some(AgentState::Waiting)
    } else {
        None
    }
}

/// What an agent's own records say it's doing.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Report {
    /// The last thing you asked it.
    pub prompt: Option<String>,
    /// Subagents it started that haven't reported back.
    pub subagents: usize,
    /// The session's name.
    pub name: Option<String>,
}

/// Reads Claude Code's records for the agent in a pane, once a second, off
/// the UI thread: transcripts run to tens of megabytes. The thread ends
/// once the watcher is dropped.
pub struct Watcher {
    shared: Arc<Mutex<Watch>>,
}

#[derive(Default)]
struct Watch {
    /// The pane's foreground process group: the agent and its launcher.
    group: i32,
    report: Option<Report>,
}

impl Watcher {
    pub fn new() -> Watcher {
        let shared = Arc::new(Mutex::new(Watch::default()));
        let theirs = shared.clone();
        let _ = std::thread::Builder::new().name("agent-watch".into()).spawn(move || watch(theirs));
        Watcher { shared }
    }

    pub fn set_group(&self, group: i32) {
        self.shared.lock().unwrap().group = group;
    }

    pub fn report(&self) -> Option<Report> {
        self.shared.lock().unwrap().report.clone()
    }
}

fn watch(shared: Arc<Mutex<Watch>>) {
    let mut transcript = Transcript::default();
    // Only the watcher's own handle left: the pane let go.
    while Arc::strong_count(&shared) > 1 {
        let group = shared.lock().unwrap().group;
        let report = (group > 0).then(|| claude_session(group)).flatten().map(|session| {
            let path = format!("{}/projects/{}/{}.jsonl", claude_dir(), project_dir(&session.cwd), session.id);
            if transcript.path != path {
                transcript = Transcript { path, ..Transcript::default() };
            }
            transcript.read_new();
            Report { prompt: transcript.prompt.clone(), subagents: transcript.subagents.len(), name: session.name }
        });
        shared.lock().unwrap().report = report;
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn claude_dir() -> String {
    std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| format!("{}/.claude", std::env::var("HOME").unwrap_or_default()))
}

/// Where Claude Code keeps a folder's transcripts: the path with every
/// character but letters and digits made a dash.
fn project_dir(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

struct ClaudeSession {
    id: String,
    cwd: String,
    name: Option<String>,
}

/// The Claude Code session run by process group `group`. Claude Code
/// writes one record per process, named for its pid: the group leader
/// when run straight from the shell, a member when run through npx.
fn claude_session(group: i32) -> Option<ClaudeSession> {
    let dir = format!("{}/sessions", claude_dir());
    let read = |pid: i32| -> Option<ClaudeSession> {
        let record = json::parse(&std::fs::read_to_string(format!("{dir}/{pid}.json")).ok()?)?;
        Some(ClaudeSession {
            id: record.str_at("sessionId")?.to_owned(),
            cwd: record.str_at("cwd")?.to_owned(),
            name: record.str_at("name").filter(|name| !name.is_empty()).map(str::to_owned),
        })
    };
    read(group).or_else(|| {
        std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.strip_suffix(".json")?.parse::<i32>().ok())
            .find(|&pid| pid != group && crate::platform::process_group(pid) == group)
            .and_then(read)
    })
}

/// A transcript read so far.
#[derive(Default)]
struct Transcript {
    path: String,
    /// How far it's been read, up to the end of its last whole line.
    offset: u64,
    prompt: Option<String>,
    /// Subagent tool calls not yet answered, and whether each runs in the
    /// background (answered at once, reporting back later).
    subagents: VecDeque<(String, bool)>,
}

impl Transcript {
    fn read_new(&mut self) {
        let Ok(mut file) = std::fs::File::open(&self.path) else { return };
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(self.offset)).is_err() || file.read_to_end(&mut bytes).is_err() {
            return;
        }
        let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else { return };
        self.offset += end as u64 + 1;
        for line in String::from_utf8_lossy(&bytes[..end]).lines() {
            self.line(line);
        }
    }

    fn line(&mut self, line: &str) {
        // Most of a transcript is tool output; look closer only at turns.
        if !line.contains("\"type\":\"user\"") && !line.contains("\"type\":\"assistant\"") {
            return;
        }
        let Some(entry) = json::parse(line) else { return };
        if entry.get("isSidechain").is_some_and(Value::is_true) {
            return;
        }
        let Some(message) = entry.get("message") else { return };
        let content = message.get("content");
        match entry.str_at("type") {
            Some("assistant") => {
                for item in content.map(Value::array).unwrap_or_default() {
                    if item.str_at("type") == Some("tool_use") && matches!(item.str_at("name"), Some("Agent" | "Task"))
                    {
                        let background = item.get("input").and_then(|input| input.get("run_in_background"));
                        let id = item.str_at("id").unwrap_or_default().to_owned();
                        self.subagents.push_back((id, background.is_some_and(Value::is_true)));
                    }
                }
            }
            Some("user") => {
                let text = match content {
                    Some(Value::String(text)) => text.clone(),
                    Some(Value::Array(items)) => {
                        let mut text = String::new();
                        for item in items {
                            match item.str_at("type") {
                                Some("tool_result") => {
                                    let id = item.str_at("tool_use_id").unwrap_or_default();
                                    self.subagents.retain(|(call, background)| call != id || *background);
                                    return;
                                }
                                Some("text") => text.push_str(item.str_at("text").unwrap_or_default()),
                                _ => {}
                            }
                        }
                        text
                    }
                    _ => return,
                };
                if text.starts_with("<task-notification>") {
                    // A background subagent reporting back: the one it names,
                    // else the oldest.
                    let named = self.subagents.iter().position(|(id, bg)| *bg && !id.is_empty() && text.contains(id));
                    if let Some(index) = named.or_else(|| self.subagents.iter().position(|(_, bg)| *bg)) {
                        self.subagents.remove(index);
                    }
                    return;
                }
                if entry.get("isMeta").is_some_and(Value::is_true) {
                    return;
                }
                if let Some(prompt) = prompt_text(&text) {
                    self.prompt = Some(prompt);
                }
            }
            _ => {}
        }
    }
}

/// What you typed, from a user turn: the slash command run, or the text
/// on one line; None for what Claude Code writes in your name.
fn prompt_text(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("<command-name>") {
        let command = rest.split("</command-name>").next()?.trim();
        let args = text.split("<command-args>").nth(1).and_then(|rest| rest.split("</command-args>").next());
        return Some(match args.map(str::trim).filter(|args| !args.is_empty()) {
            Some(args) => format!("{command} {args}"),
            None => command.to_owned(),
        });
    }
    if let Some(rest) = text.strip_prefix("<bash-input>") {
        return Some(format!("! {}", rest.split("</bash-input>").next()?.trim()));
    }
    if text.is_empty()
        || text.starts_with('<')
        || text.starts_with("Caveat:")
        || text.starts_with("[Request interrupted")
    {
        return None;
    }
    Some(one_line(text))
}

/// `text` with every run of whitespace, newlines too, made one space.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_names_skip_launchers_paths_and_versions() {
        assert_eq!(program_name("FOO=1 sudo /usr/bin/htop -d 5"), Some("htop"));
        assert_eq!(program_name("npx @anthropic-ai/claude-code@latest"), Some("claude-code"));
        assert_eq!(program_name("env"), None);
    }

    #[test]
    fn agent_commands_are_recognized_through_launchers() {
        assert!(is_agent_command("claude"));
        assert!(is_agent_command("haikucode"));
        assert!(is_agent_command("agent -p \"fix the build\""));
        assert!(is_agent_command("claude --resume"));
        assert!(is_agent_command("/opt/homebrew/bin/codex"));
        assert!(is_agent_command("npx @anthropic-ai/claude-code@latest"));
        assert!(is_agent_command("ANTHROPIC_MODEL=x claude -p hi"));
        assert!(is_agent_command("pnpm dlx @google/gemini-cli"));
        assert!(is_agent_command("bunx opencode"));
        assert!(!is_agent_command("ls -la"));
        assert!(!is_agent_command("vim claude.md"));
        assert!(!is_agent_command(""));
    }

    #[test]
    fn titles_and_screens_read_as_states() {
        assert_eq!(title_state("⠐ Fixing the build"), Some(AgentState::Running));
        assert_eq!(title_state("✳ Fixing the build"), Some(AgentState::Idle));
        assert_eq!(title_state("✋  Action Required (temino)"), Some(AgentState::Waiting));
        assert_eq!(title_state("zsh"), None);
        assert_eq!(screen_state("✻ Thinking… (esc to interrupt)"), Some(AgentState::Running));
        assert_eq!(screen_state("Do you want to proceed?\n❯ 1. Yes\n  2. No"), Some(AgentState::Waiting));
        assert_eq!(screen_state("> "), None);
        assert_eq!(screen_state("approve bash: rm -rf target  (Y)es  (N)o"), Some(AgentState::Waiting));
        assert_eq!(screen_state("(Esc to interrupt · Enter queues)"), Some(AgentState::Running));
    }

    #[test]
    fn transcripts_give_the_last_prompt_and_open_subagents() {
        let mut transcript = Transcript::default();
        for line in [
            r#"{"type":"user","message":{"role":"user","content":"fix the\nbuild"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"Caveat: ignore"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Agent","input":{}},{"type":"tool_use","id":"b","name":"Agent","input":{"run_in_background":true}},{"type":"tool_use","id":"c","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"b","content":"launched"}]}}"#,
        ] {
            transcript.line(line);
        }
        assert_eq!(transcript.prompt.as_deref(), Some("fix the build"));
        assert_eq!(transcript.subagents.len(), 2);
        transcript
            .line(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a","content":"ok"}]}}"#);
        transcript.line(r#"{"type":"user","message":{"content":"<task-notification>b done</task-notification>"}}"#);
        assert!(transcript.subagents.is_empty());
        transcript.line(r#"{"type":"user","message":{"content":"<command-name>/review</command-name><command-args>12</command-args>"}}"#);
        assert_eq!(transcript.prompt.as_deref(), Some("/review 12"));
        assert_eq!(project_dir("/Users/a/my.app"), "-Users-a-my-app");
    }
}

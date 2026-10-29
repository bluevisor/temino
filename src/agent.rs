//! Spotting a coding agent (Claude Code, Codex, Gemini, ...) in a pane and
//! reading what it's up to from its command, its window title and what it
//! leaves on screen.

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
const AGENTS: [&str; 14] = [
    "claude",
    "claude-code",
    "codex",
    "gemini",
    "aider",
    "opencode",
    "cursor-agent",
    "amp",
    "goose",
    "crush",
    "qwen",
    "copilot",
    "droid",
    "kiro-cli",
];

/// Words that run the next word as the program.
const LAUNCHERS: [&str; 10] = ["env", "npx", "bunx", "pnpx", "exec", "command", "nohup", "time", "sudo", "uvx"];

/// Whether `command` runs an agent: `claude`, `npx @openai/codex`,
/// `FOO=1 gemini -p ...`.
pub fn is_agent_command(command: &str) -> bool {
    let mut words = command.split_whitespace().filter(|word| !word.starts_with('-'));
    let program = loop {
        match words.next() {
            Some(word) if word.contains('=') || LAUNCHERS.contains(&word) || word == "dlx" => continue,
            Some("pnpm" | "yarn" | "bun" | "npm") => continue,
            Some(word) => break word,
            None => return false,
        }
    };
    let name = program.rsplit('/').next().unwrap_or(program);
    // `@anthropic-ai/claude-code@latest` → `claude-code`.
    let name = name.split('@').find(|part| !part.is_empty()).unwrap_or(name);
    let name = name.strip_suffix("-cli").unwrap_or(name);
    AGENTS.contains(&name) || name == "kiro"
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
    const ASKING: [&str; 12] = [
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_commands_are_recognized_through_launchers() {
        assert!(is_agent_command("claude"));
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
    }
}

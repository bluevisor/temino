//! Bringing back, at launch, what each pane was running at quit: a coding
//! agent picks its conversation back up; a monitor, a session manager, a
//! log, a remote shell or an editor starts again, and with their own
//! setting, dev servers. Anything else isn't run again on its own (Up
//! recalls it): a second run could do harm.

use crate::agent;

mod rerun;

/// Which setting brings a line back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Settings ▸ Reopen running programs: agents, monitors, tmux, logs,
    /// ssh, editors.
    Program,
    /// Settings ▸ Restart dev servers.
    Server,
}

impl Kind {
    pub fn code(self) -> &'static str {
        match self {
            Kind::Program => "P",
            Kind::Server => "S",
        }
    }

    pub fn from_code(code: &str) -> Option<Kind> {
        match code {
            "P" => Some(Kind::Program),
            "S" => Some(Kind::Server),
            _ => None,
        }
    }

    /// Whether its setting is on; both are off unless turned on.
    pub fn enabled(self) -> bool {
        let key = match self {
            Kind::Program => "resume_programs",
            Kind::Server => "resume_servers",
        };
        crate::setting(key).as_deref() == Some("on")
    }
}

/// The line that brings back what `command` was running, and which
/// setting it's under: an agent's conversation (`program` is its entry in
/// AGENTS, `claude_id` Claude Code's session), or a program that's safe
/// to run again.
pub fn line(command: &str, program: Option<&str>, claude_id: Option<&str>) -> Option<(Kind, String)> {
    let command = command.trim();
    if command.is_empty() || command.contains('\n') {
        return None;
    }
    match program {
        Some(program) => resume_line(command, program, claude_id).map(|line| (Kind::Program, line)),
        None => rerun::line(command),
    }
}

/// The words of a command line, quotes kept, split where the shell would.
fn shell_words(line: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let (mut start, mut quote, mut escaped) = (None, None, false);
    for (at, c) in line.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
            start.get_or_insert(at);
        } else if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            start.get_or_insert(at);
        } else if c.is_whitespace() {
            if let Some(from) = start.take() {
                words.push(&line[from..at]);
            }
        } else {
            start.get_or_insert(at);
        }
    }
    if let Some(from) = start {
        words.push(&line[from..]);
    }
    words
}

/// How an agent from AGENTS picks its last conversation back up.
struct Comeback {
    /// A subcommand that goes right after the program: `codex resume`.
    sub: Option<&'static str>,
    /// What goes at the end: `--last`, `--continue`.
    then: &'static str,
    /// Its short flags that pick, start or print a session, to leave off
    /// (`-c` continues in Claude Code, but sets config in Codex).
    shorts: &'static [&'static str],
}

/// Each agent's own way back to its latest conversation in the folder it
/// ran in. Amp has none: it only takes a thread's id.
fn comeback(program: &str) -> Option<Comeback> {
    let way = |sub, then, shorts| Some(Comeback { sub, then, shorts });
    match program {
        "claude" | "claude-code" => way(None, "--continue", &["-c", "-r", "-p"]),
        "codex" => way(Some("resume"), "--last", &[]),
        "cursor-agent" | "agent" => way(None, "--continue", &["-p"]),
        "gemini" => way(None, "--resume latest", &["-r", "-p"]),
        "qwen" | "pi" => way(None, "--continue", &["-c", "-r", "-p"]),
        "opencode" => way(None, "--continue", &["-c", "-s"]),
        "haikucode" => way(None, "--continue", &["-c", "-r"]),
        "copilot" => way(None, "--continue", &["-p"]),
        "crush" => way(None, "--continue", &["-C", "-s"]),
        "droid" => way(None, "--resume", &["-r"]),
        "kiro-cli" => way(Some("chat"), "--resume", &["-r"]),
        "goose" => way(Some("session"), "--resume", &["-r"]),
        "aider" => way(None, "--restore-chat-history", &[]),
        _ => None,
    }
}

/// Where the program is among a command's words: past variables,
/// launchers and package runners, as `program_name` goes.
fn program_index(words: &[&str]) -> Option<usize> {
    words.iter().position(|word| {
        !(word.starts_with(['-', '"', '\''])
            || word.contains('=')
            || agent::LAUNCHERS.contains(word)
            || matches!(*word, "dlx" | "pnpm" | "yarn" | "bun" | "npm"))
    })
}

/// The line that brings back the conversation agent `program` (an entry
/// in AGENTS) was having, run the way `command` started it: `claude-max`
/// → `claude-max --resume <id>` with Claude Code's session id, else the
/// agent's own way to its latest one (`codex resume --last`). Flags that
/// pick, start or print a session and a quoted prompt are left off, so
/// the conversation picks up instead of starting over or asking again.
pub fn resume_line(command: &str, program: &str, claude_id: Option<&str>) -> Option<String> {
    let way = comeback(program)?;
    let id = claude_id.filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    let then = match id {
        Some(id) if matches!(program, "claude" | "claude-code") => format!("--resume {id}"),
        _ => way.then.to_owned(),
    };
    let words = shell_words(command);
    let at = program_index(&words)?;
    let mut kept: Vec<&str> = words[..=at].to_vec();
    kept.extend(way.sub);
    let mut rest = words[at + 1..].iter().copied().peekable();
    let mut sub_seen = false;
    while let Some(word) = rest.next() {
        let short = way.shorts.contains(&word);
        let takes_value =
            matches!(word, "--resume" | "--session-id" | "--session" | "--resume-id" | "--print" | "--prompt")
                || (short && matches!(word, "-r" | "-s" | "-p"));
        let bare = matches!(
            word,
            "--continue" | "--fork-session" | "--fork" | "--last" | "--resume-picker" | "--restore-chat-history"
        ) || short;
        let valued = ["--resume=", "--session-id=", "--session=", "--resume-id=", "--prompt="];
        if takes_value || (way.sub == Some("resume") && !sub_seen && word == "resume") {
            sub_seen |= word == "resume";
            if rest.peek().is_some_and(|next| !next.starts_with('-')) {
                rest.next();
            }
        } else if way.sub == Some(word) && !sub_seen {
            sub_seen = true;
        } else if !(bare
            || valued.iter().any(|prefix| word.starts_with(prefix))
            || word.starts_with('"')
            || word.starts_with('\''))
        {
            kept.push(word);
        }
    }
    kept.push(&then);
    Some(kept.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_resumes_its_session_the_way_it_was_started() {
        let id = "aab4bdc0-7e49-429e-9e90-19435c1ed9e6";
        let claude = |command| resume_line(command, "claude", Some(id)).unwrap();
        assert_eq!(claude("claude-max"), format!("claude-max --resume {id}"));
        assert_eq!(claude("claude --model opus --resume old-id -c"), format!("claude --model opus --resume {id}"));
        assert_eq!(claude("claude -r"), format!("claude --resume {id}"));
        assert_eq!(
            claude("FOO=\"a b\" claude --session-id=x -p \"fix the 'build'\" --verbose"),
            format!("FOO=\"a b\" claude --verbose --resume {id}")
        );
        assert_eq!(claude("claude 'what next?'"), format!("claude --resume {id}"));
        // Without a usable id, its latest conversation here.
        assert_eq!(resume_line("claude", "claude", Some("x; rm -rf ~")).unwrap(), "claude --continue");
        assert_eq!(resume_line("claude", "claude", None).unwrap(), "claude --continue");
        assert_eq!(resume_line("\"hi\"", "claude", Some(id)), None);
        assert_eq!(shell_words(r#"a "b c" 'd \e' f\ g"#), ["a", "\"b c\"", "'d \\e'", "f\\ g"]);
    }

    #[test]
    fn every_agent_picks_its_latest_conversation_back_up() {
        let line = |command: &str| {
            let program = agent::agent_program(command).unwrap();
            resume_line(command, program, None)
        };
        assert_eq!(line("codex --yolo -c model=o3").unwrap(), "codex resume --yolo -c model=o3 --last");
        assert_eq!(line("codex resume 0199-ab --last").unwrap(), "codex resume --last");
        assert_eq!(line("npx -y @openai/codex \"fix it\"").unwrap(), "npx -y @openai/codex resume --last");
        assert_eq!(line("gemini -m pro -p \"hi\" --resume 5").unwrap(), "gemini -m pro --resume latest");
        assert_eq!(line("agent --resume abc").unwrap(), "agent --continue");
        assert_eq!(line("cursor-agent").unwrap(), "cursor-agent --continue");
        assert_eq!(line("opencode -s ses_1 -m x").unwrap(), "opencode -m x --continue");
        assert_eq!(line("pi -c").unwrap(), "pi --continue");
        assert_eq!(line("qwen --yolo").unwrap(), "qwen --yolo --continue");
        assert_eq!(line("haikucode -r fix-build").unwrap(), "haikucode --continue");
        assert_eq!(line("copilot --allow-all-tools").unwrap(), "copilot --allow-all-tools --continue");
        assert_eq!(line("crush -c ~/src -C").unwrap(), "crush -c ~/src --continue");
        assert_eq!(line("droid -r 12").unwrap(), "droid --resume");
        assert_eq!(line("kiro-cli chat --resume-picker").unwrap(), "kiro-cli chat --resume");
        assert_eq!(line("kiro-cli").unwrap(), "kiro-cli chat --resume");
        assert_eq!(line("goose session -n work").unwrap(), "goose session -n work --resume");
        assert_eq!(line("aider --model sonnet").unwrap(), "aider --model sonnet --restore-chat-history");
        assert_eq!(line("amp"), None);
    }

    #[test]
    fn agents_come_back_as_programs() {
        assert_eq!(line("  ", Some("claude"), None), None);
        assert_eq!(
            line("claude-max", Some("claude"), Some("1-2")),
            Some((Kind::Program, "claude-max --resume 1-2".to_owned()))
        );
        assert_eq!(line("claude\nmore", Some("claude"), Some("1-2")), None);
    }
}

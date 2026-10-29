//! AI mode: a request in plain words becomes either a shell command to
//! review in the editor, or an answer shown in a block. TypeSafe's Jev
//! decides which, when there's a key for it; the language model writes it.

use crate::json::{self, Value};
use crate::llm;

const COMMAND_SYSTEM: &str = "You are built into a terminal. The user described, in plain words, something \
to do. Reply with exactly one line: COMMAND: followed by a single command line for the user's shell that does \
it, with nothing else: no explanation, no code fences. Use the files listed when the request names one loosely.";

const ANSWER_SYSTEM: &str = "You are built into a terminal. The user asked a question. Reply with ANSWER: \
followed by a short plain-text answer, at most a few lines, no markdown headings. Show a command when one \
helps, on its own line.";

/// For when nothing decided beforehand: the model picks.
const EITHER_SYSTEM: &str = "You are built into a terminal. The user typed a request in plain words.\n\
If it describes something to do that a shell command can do (copy, move or rename files, find something, \
install, start a program), reply with exactly one line: COMMAND: followed by a single command line for the \
user's shell, with nothing else: no explanation, no code fences. Use the files listed when the request names \
one loosely.\n\
Otherwise (a question, an explanation), reply with ANSWER: followed by a short plain-text answer, at most a \
few lines, no markdown headings.";

/// Room for an answer of a few lines.
const MAX_TOKENS: u32 = 400;

#[derive(Debug, PartialEq)]
pub enum Reply {
    Command(String),
    Answer(String),
}

/// What the model is told besides the request: where the user is and what
/// they just ran, so "this file" and "my downloads" mean something.
pub fn prompt(request: &str, shell: &str, cwd: &str, recent: &[String]) -> String {
    let shell = shell.rsplit('/').next().unwrap_or(shell);
    let mut names: Vec<String> = std::fs::read_dir(cwd)
        .map(|entries| entries.flatten().filter_map(|e| e.file_name().to_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    names.retain(|name| !name.starts_with('.'));
    names.sort();
    names.truncate(80);
    let home = std::env::var("HOME").unwrap_or_default();
    let mut out = format!(
        "System: {}\nShell: {shell}\nHome: {home}\nWorking directory: {cwd}\nFiles here: {}\n",
        crate::platform::OS_NAME,
        if names.is_empty() { "(none)".to_owned() } else { names.join(", ") },
    );
    if !recent.is_empty() {
        out.push_str(&format!("Recent commands, newest last: {}\n", recent.join(" ; ")));
    }
    out.push_str(&format!("\nRequest: {request}"));
    out
}

const JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";
const JEV_QUESTION: &str = "Someone typed `request` into a terminal. Do they want something done on their \
computer that one shell command can do right now, or do they want an explanation?";
/// Below this, Jev's pick is a coin toss and the model decides instead.
const JEV_CONFIDENCE: f64 = 0.6;

/// TypeSafe's key: `TYPESAFE_API_KEY`, then the `typesafe_key` setting.
fn jev_key() -> Option<String> {
    std::env::var("TYPESAFE_API_KEY").ok().or_else(|| crate::setting("typesafe_key")).filter(|key| !key.is_empty())
}

/// Whether `request` asks for a command (true) or an answer (false), when
/// Jev is set up and sure. Only the request is sent: no file names, no
/// output. Blocks: run it off the UI thread.
fn wants_command(request: &str) -> Option<bool> {
    let key = jev_key()?;
    let body = format!(
        r#"{{"model":"jev-latest","state":{{"request":{}}},"questions":{{"kind":{{"type":"choice","instructions":{},"criteria":{{"command":{},"answer":{}}}}}}}}}"#,
        llm::quote(request),
        llm::quote(JEV_QUESTION),
        llm::quote(
            "They describe an action to carry out now: copy, move, rename, delete, find, install, start, stop \
             or inspect something. A shell command does it."
        ),
        llm::quote("They ask a question or want something explained; nothing should run."),
    );
    let reply = json::parse(&llm::post_with_key(JEV_URL, &key, &body, 10).ok()?)?;
    let kind = reply.get("answers")?.get("kind")?;
    let confidence = match kind.get("confidence")? {
        Value::Number(n) => *n,
        _ => return None,
    };
    (confidence >= JEV_CONFIDENCE).then(|| kind.str_at("choice") == Some("command"))
}

/// The whole round trip for AI mode's Enter: Jev's pick, then the model's
/// command or answer. Blocks.
pub fn run(request: &str, prompt: &str) -> Result<Reply, String> {
    let system = match wants_command(request) {
        Some(true) => COMMAND_SYSTEM,
        Some(false) => ANSWER_SYSTEM,
        None => EITHER_SYSTEM,
    };
    llm::complete(system, prompt, MAX_TOKENS).map(|reply| parse(&reply))
}

/// Reads the model's reply: a command when it says COMMAND:, else an answer.
pub fn parse(reply: &str) -> Reply {
    let text = reply.trim();
    let unfenced = text.trim_start_matches("```").trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let unfenced = unfenced.trim_end_matches("```").trim();
    for candidate in [text, unfenced] {
        if let Some(rest) = candidate.strip_prefix("COMMAND:") {
            let command = rest.trim().trim_matches('`').trim();
            // One line: a model that explains anyway explains below it.
            let command = command.lines().next().unwrap_or("").trim();
            if !command.is_empty() {
                return Reply::Command(command.to_owned());
            }
        }
    }
    Reply::Answer(text.strip_prefix("ANSWER:").unwrap_or(text).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_read_as_commands_or_answers() {
        assert_eq!(
            parse("COMMAND: mv example.jpg example_backup.jpg"),
            Reply::Command("mv example.jpg example_backup.jpg".into())
        );
        assert_eq!(parse("```\nCOMMAND: `cp a.txt ~/Downloads/`\n```"), Reply::Command("cp a.txt ~/Downloads/".into()));
        assert_eq!(parse("COMMAND: ls -la\nThis lists files."), Reply::Command("ls -la".into()));
        assert_eq!(
            parse("ANSWER: A symlink points at another path."),
            Reply::Answer("A symlink points at another path.".into())
        );
        assert_eq!(parse("Just text"), Reply::Answer("Just text".into()));
    }

    /// Against the real services: `TYPESAFE_API_KEY=… cargo test live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_requests_route_and_answer() {
        let prompt = |request: &str| prompt(request, "/bin/zsh", "/tmp", &[]);
        for (request, command) in
            [("rename example.jpg to example_backup.jpg", true), ("what does chmod 755 mean?", false)]
        {
            assert_eq!(wants_command(request), Some(command), "{request}");
            let reply = run(request, &prompt(request)).unwrap();
            println!("{request} -> {reply:?}");
            assert_eq!(matches!(reply, Reply::Command(_)), command);
        }
    }
}

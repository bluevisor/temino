//! `temino <command>`: drives the running Temino through its API (api.rs)
//! from a shell, a script or an agent; `temino mcp` serves the same as an
//! MCP server on stdin and stdout, for agents that speak MCP.
//!
//! Inside a Temino pane the pane and window default to that pane's own
//! (TEMINO_PANE, TEMINO_WINDOW); `--pane` and `--window` pick others.

use std::io::{BufRead, Write};

use crate::api::{self, write};
use crate::json::{self, Value};
use crate::remote::quote;

/// The words that make `temino` a client instead of the app.
pub const COMMANDS: [&str; 13] =
    ["list", "new-tab", "split", "focus", "close", "send", "keys", "run", "read", "notify", "api", "mcp", "help"];

const HELP: &str = "\
temino: drive the running Temino from a shell, a script or an agent

  temino list                              windows, tabs and panes, as JSON
  temino new-tab [--cwd DIR] [COMMAND…]    a new tab, running COMMAND
  temino split [--down] [COMMAND…]         split the pane, running COMMAND
  temino focus | close                     the pane
  temino send TEXT…                        type TEXT into the pane
  temino keys KEY…                         press keys: Enter, ctrl+c, shift+tab, F5…
  temino run [--wait] [--timeout S] COMMAND…
                                           run COMMAND at the pane's prompt; --wait
                                           prints its output and exits with its status
  temino read [--blocks N] [--lines N] [--json]
                                           the pane's last commands and their output
  temino notify TITLE [BODY…]              a desktop notification
  temino api METHOD [JSON]                 any API method, raw
  temino mcp                               an MCP server on stdin/stdout, for agents

  --pane N, --window N pick the pane (default: the one this runs in, from
  TEMINO_PANE and TEMINO_WINDOW). TEMINO_SOCKET points at another Temino.
";

/// Runs a CLI command; the process's exit status.
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("mcp") => mcp(),
        Some("help") | None => {
            print!("{HELP}");
            0
        }
        Some(command) => match command_line(command, &args[1..]) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("temino {command}: {error}");
                1
            }
        },
    }
}

/// Options and the words after them.
struct Args {
    window: Option<u32>,
    pane: Option<u64>,
    flags: Vec<String>,
    values: Vec<(String, String)>,
    words: Vec<String>,
}

impl Args {
    fn parse(args: &[String], takes_value: &[&str]) -> Result<Args, String> {
        let mut out = Args { window: None, pane: None, flags: Vec::new(), values: Vec::new(), words: Vec::new() };
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--" => {
                    out.words.extend(rest.by_ref().cloned());
                    break;
                }
                "--pane" | "--window" => {
                    let value = rest.next().ok_or(format!("{arg} needs a number"))?;
                    let n: u64 = value.parse().map_err(|_| format!("{arg} needs a number, not {value}"))?;
                    if arg == "--pane" {
                        out.pane = Some(n);
                    } else {
                        out.window = Some(n as u32);
                    }
                }
                flag if flag.starts_with("--") && takes_value.contains(&flag) => {
                    let value = rest.next().ok_or(format!("{flag} needs a value"))?;
                    out.values.push((flag.to_owned(), value.clone()));
                }
                flag if flag.starts_with("--") && out.words.is_empty() => out.flags.push(flag.to_owned()),
                word => out.words.push(word.to_owned()),
            }
        }
        Ok(out)
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The window, and the pane where it matters, as JSON fields.
    fn target(&self, pane: bool) -> Result<String, String> {
        let env = |key| std::env::var(key).ok().and_then(|v| v.parse::<u64>().ok());
        let window = self.window.map(u64::from).or_else(|| env("TEMINO_WINDOW")).unwrap_or(1);
        let mut fields = format!(r#""window":{window}"#);
        if pane {
            let pane = self.pane.or_else(|| env("TEMINO_PANE")).ok_or("which pane? (--pane N, outside Temino)")?;
            fields.push_str(&format!(r#","pane":{pane}"#));
        }
        Ok(fields)
    }
}

fn command_line(command: &str, args: &[String]) -> Result<i32, String> {
    let args = Args::parse(args, &["--cwd", "--timeout", "--blocks", "--lines"])?;
    let joined = args.words.join(" ");
    let print = |result: String| {
        println!("{result}");
        Ok(0)
    };
    match command {
        "list" => print(api::call("list", "{}")?),
        "new-tab" => {
            let mut params = args.target(false)?;
            if let Some(cwd) = args.value("--cwd") {
                params.push_str(&format!(r#","cwd":{}"#, quote(cwd)));
            }
            if !joined.is_empty() {
                params.push_str(&format!(r#","command":{}"#, quote(&joined)));
            }
            print(api::call("new_tab", &format!("{{{params}}}"))?)
        }
        "split" => {
            let direction = if args.flag("--down") { "down" } else { "right" };
            let mut params = format!(r#"{},"direction":"{direction}""#, args.target(true)?);
            if !joined.is_empty() {
                params.push_str(&format!(r#","command":{}"#, quote(&joined)));
            }
            print(api::call("split", &format!("{{{params}}}"))?)
        }
        "focus" | "close" => print(api::call(command, &format!("{{{}}}", args.target(true)?))?),
        "send" => {
            let params = format!(r#"{{{},"text":{}}}"#, args.target(true)?, quote(&joined));
            print(api::call("send_text", &params)?)
        }
        "keys" => {
            let keys: Vec<String> = args.words.iter().map(|k| quote(k)).collect();
            let params = format!(r#"{{{},"keys":[{}]}}"#, args.target(true)?, keys.join(","));
            print(api::call("send_keys", &params)?)
        }
        "run" => {
            if joined.trim().is_empty() {
                return Err("what command?".into());
            }
            let wait = args.flag("--wait");
            let timeout =
                args.value("--timeout").map_or(Ok(120.0), |t| t.parse::<f64>().map_err(|_| "--timeout is seconds"))?;
            let params =
                format!(r#"{{{},"command":{},"wait":{wait},"timeout":{timeout}}}"#, args.target(true)?, quote(&joined));
            let result = api::call("run", &params)?;
            if !wait {
                return print(result);
            }
            // Waited: its output, and its status as ours.
            let block = json::parse(&result).ok_or("the answer isn't JSON")?;
            if let Some(output) = block.str_at("output").filter(|o| !o.is_empty()) {
                println!("{output}");
            }
            if block.get("running").is_some_and(Value::is_true) {
                eprintln!("temino run: still running after {timeout}s");
                return Ok(124);
            }
            Ok(block.num_at("exit").map_or(0, |e| e as i32))
        }
        "read" => {
            let mut params = args.target(true)?;
            for (flag, key) in [("--blocks", "blocks"), ("--lines", "lines")] {
                if let Some(n) = args.value(flag) {
                    let n: u64 = n.parse().map_err(|_| format!("{flag} needs a number"))?;
                    params.push_str(&format!(r#","{key}":{n}"#));
                }
            }
            let result = api::call("read", &format!("{{{params}}}"))?;
            if args.flag("--json") {
                return print(result);
            }
            let read = json::parse(&result).ok_or("the answer isn't JSON")?;
            for block in read.get("blocks").map(Value::array).unwrap_or_default() {
                println!("$ {}", block.str_at("command").unwrap_or_default());
                if let Some(output) = block.str_at("output").filter(|o| !o.is_empty()) {
                    println!("{output}");
                }
            }
            Ok(0)
        }
        "notify" => {
            let title = args.words.first().cloned().unwrap_or_else(|| "Temino".to_owned());
            let body = args.words.get(1..).map(|w| w.join(" ")).unwrap_or_default();
            print(api::call("notify", &format!(r#"{{"title":{},"body":{}}}"#, quote(&title), quote(&body)))?)
        }
        "api" => {
            let method = args.words.first().ok_or("which method?")?;
            let params = args.words.get(1).map_or("{}", String::as_str);
            json::parse(params).ok_or("the params aren't JSON")?;
            print(api::call(method, params)?)
        }
        other => Err(format!("{other} isn't a temino command (temino help)")),
    }
}

// ---- MCP ------------------------------------------------------------------

/// The tools, in MCP's shape: name, what it's for, its input's JSON schema.
const TOOLS: &[(&str, &str, &str)] = &[
    (
        "list_panes",
        "Temino's windows, tabs and panes: each pane's id, title, folder, whether it has focus, what's running and what an agent there is doing.",
        r#"{"type":"object","properties":{}}"#,
    ),
    (
        "new_tab",
        "Open a new tab in Temino, optionally in a folder and running a command. Returns its window and pane.",
        r#"{"type":"object","properties":{"window":{"type":"integer"},"cwd":{"type":"string","description":"Folder to start in"},"command":{"type":"string","description":"Command to run once the shell is ready"}}}"#,
    ),
    (
        "split_pane",
        "Split a pane (right or down) and optionally run a command in the new one. Returns the new pane.",
        r#"{"type":"object","properties":{"window":{"type":"integer"},"pane":{"type":"integer"},"direction":{"type":"string","enum":["right","down"]},"command":{"type":"string"}}}"#,
    ),
    (
        "run_command",
        "Run a shell command at a pane's prompt, as if typed. With wait (the default) returns its exit status and output once it finishes or the timeout passes.",
        r#"{"type":"object","properties":{"window":{"type":"integer"},"pane":{"type":"integer"},"command":{"type":"string"},"wait":{"type":"boolean","default":true},"timeout":{"type":"number","description":"Seconds to wait, 120 by default"}},"required":["command"]}"#,
    ),
    (
        "read_pane",
        "Read a pane's last commands and their output (the end of each, up to lines).",
        r#"{"type":"object","properties":{"window":{"type":"integer"},"pane":{"type":"integer"},"blocks":{"type":"integer","default":1},"lines":{"type":"integer","default":200}}}"#,
    ),
    (
        "send_text",
        "Type text into a pane: into its command editor at the prompt, or to the program running there.",
        r#"{"type":"object","properties":{"window":{"type":"integer"},"pane":{"type":"integer"},"text":{"type":"string"}},"required":["text"]}"#,
    ),
    (
        "send_keys",
        "Press keys in a pane, e.g. [\"Enter\"], [\"ctrl+c\"], [\"Up\", \"Enter\"], [\"shift+tab\"].",
        r#"{"type":"object","properties":{"window":{"type":"integer"},"pane":{"type":"integer"},"keys":{"type":"array","items":{"type":"string"}}},"required":["keys"]}"#,
    ),
    (
        "focus_pane",
        "Bring a pane's tab forward and give it the keyboard.",
        r#"{"type":"object","properties":{"window":{"type":"integer"},"pane":{"type":"integer"}},"required":["pane"]}"#,
    ),
    (
        "close_pane",
        "Close a pane (its tab when it's the last one).",
        r#"{"type":"object","properties":{"window":{"type":"integer"},"pane":{"type":"integer"}},"required":["pane"]}"#,
    ),
    (
        "notify",
        "Show a desktop notification, to get the user's attention.",
        r#"{"type":"object","properties":{"title":{"type":"string"},"body":{"type":"string"}},"required":["title"]}"#,
    ),
];

/// The API method behind a tool.
fn method_of(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "list_panes" => "list",
        "new_tab" => "new_tab",
        "split_pane" => "split",
        "run_command" => "run",
        "read_pane" => "read",
        "send_text" => "send_text",
        "send_keys" => "send_keys",
        "focus_pane" => "focus",
        "close_pane" => "close",
        "notify" => "notify",
        _ => return None,
    })
}

/// Serves MCP (JSON-RPC 2.0, a message a line) until stdin closes.
fn mcp() -> i32 {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = mcp_message(&line) {
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
        }
    }
    0
}

/// The answer to one MCP message; None for notifications.
fn mcp_message(line: &str) -> Option<String> {
    let Some(message) = json::parse(line) else {
        return Some(r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#.to_owned());
    };
    // Notifications (no id) get no answer.
    let id = write(message.get("id")?);
    let params = message.get("params");
    let result = match message.str_at("method").unwrap_or_default() {
        "initialize" => {
            let version = params.and_then(|p| p.str_at("protocolVersion")).unwrap_or("2025-06-18");
            Ok(format!(
                r#"{{"protocolVersion":{},"capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"temino","version":{}}},"instructions":{}}}"#,
                quote(version),
                quote(env!("CARGO_PKG_VERSION")),
                quote("Drives the Temino terminal the user is running: open tabs and splits, run commands and read their output, type and press keys in panes, and notify the user. Panes are numbered per window; without a pane the tools use the one this server runs in, when it runs inside Temino.")
            ))
        }
        "ping" => Ok("{}".to_owned()),
        "tools/list" => {
            let tools: Vec<String> = TOOLS
                .iter()
                .map(|(name, description, schema)| {
                    format!(r#"{{"name":{},"description":{},"inputSchema":{schema}}}"#, quote(name), quote(description))
                })
                .collect();
            Ok(format!(r#"{{"tools":[{}]}}"#, tools.join(",")))
        }
        "tools/call" => {
            let name = params.and_then(|p| p.str_at("name")).unwrap_or_default();
            match method_of(name) {
                None => Err((-32602, format!("unknown tool {name}"))),
                Some(method) => Ok(tool_result(method, params.and_then(|p| p.get("arguments")))),
            }
        }
        other => Err((-32601, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(result) => format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#),
        Err((code, error)) => {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":{}}}}}"#, quote(&error))
        }
    })
}

/// A tool's result: what Temino answered, or why it couldn't, as text.
fn tool_result(method: &str, arguments: Option<&Value>) -> String {
    let mut fields: Vec<(String, Value)> = match arguments {
        Some(Value::Object(fields)) => fields.iter().map(|(k, v)| (k.clone(), clone(v))).collect(),
        _ => Vec::new(),
    };
    // The pane this server runs in, where none was named.
    let env = |key| std::env::var(key).ok().and_then(|v| v.parse::<f64>().ok());
    for (key, var) in [("window", "TEMINO_WINDOW"), ("pane", "TEMINO_PANE")] {
        if !fields.iter().any(|(k, _)| k == key) {
            if let Some(n) = env(var) {
                fields.push((key.to_owned(), Value::Number(n)));
            }
        }
    }
    // run_command waits unless told not to.
    if method == "run" && !fields.iter().any(|(k, _)| k == "wait") {
        fields.push(("wait".to_owned(), Value::Bool(true)));
    }
    let (text, error) = match api::call(method, &write(&Value::Object(fields))) {
        Ok(result) => (result, false),
        Err(error) => (error, true),
    };
    format!(r#"{{"content":[{{"type":"text","text":{}}}],"isError":{error}}}"#, quote(&text))
}

fn clone(value: &Value) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Number(n) => Value::Number(*n),
        Value::String(s) => Value::String(s.clone()),
        Value::Array(items) => Value::Array(items.iter().map(clone).collect()),
        Value::Object(fields) => Value::Object(fields.iter().map(|(k, v)| (k.clone(), clone(v))).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_says_who_it_is_and_what_it_offers() {
        let init =
            mcp_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#)
                .unwrap();
        let init = json::parse(&init).unwrap();
        let result = init.get("result").unwrap();
        assert_eq!(result.str_at("protocolVersion"), Some("2025-03-26"));
        assert!(result.get("capabilities").and_then(|c| c.get("tools")).is_some());
        assert_eq!(mcp_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), None);
        let list = json::parse(&mcp_message(r#"{"jsonrpc":"2.0","id":"a","method":"tools/list"}"#).unwrap()).unwrap();
        let tools = list.get("result").and_then(|r| r.get("tools")).map(Value::array).unwrap();
        assert_eq!(tools.len(), TOOLS.len());
        assert!(tools.iter().all(|t| method_of(t.str_at("name").unwrap()).is_some()));
        // Every schema is JSON.
        assert!(TOOLS.iter().all(|(_, _, schema)| json::parse(schema).is_some()));
    }

    #[test]
    fn mcp_errors_are_json_rpc_errors() {
        let unknown = json::parse(&mcp_message(r#"{"jsonrpc":"2.0","id":7,"method":"nope"}"#).unwrap()).unwrap();
        assert_eq!(unknown.get("error").and_then(|e| e.num_at("code")), Some(-32601.0));
        let bad = json::parse(&mcp_message("{").unwrap()).unwrap();
        assert_eq!(bad.get("error").and_then(|e| e.num_at("code")), Some(-32700.0));
    }

    #[test]
    fn options_come_before_the_words() {
        let args: Vec<String> = ["--pane", "3", "--wait", "make", "--jobs", "4"].map(String::from).to_vec();
        let parsed = Args::parse(&args, &["--timeout"]).unwrap();
        assert_eq!(parsed.pane, Some(3));
        assert!(parsed.flag("--wait"));
        assert_eq!(parsed.words, ["make", "--jobs", "4"]);
    }
}

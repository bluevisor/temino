//! A language model for the few things that want one (the tab recap's
//! summary), over any OpenAI-compatible server. Calls block: run them off
//! the UI thread. curl does the HTTP, as for updates.

use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::json;

/// The development model server, until there's a chosen one: on the
/// tailnet only, so off it every call just fails and nothing is shown.
const DEV_URL: &str = "http://jetson-orin.heron-minor.ts.net:8080/v1";

/// The server's model, once asked: the first it lists, unless set.
static MODEL: Mutex<Option<String>> = Mutex::new(None);

/// The server's base URL: `TEMINO_LLM_URL`, then the `llm_url` setting,
/// then the development server. `off` turns the model off.
fn base_url() -> Option<String> {
    let url = std::env::var("TEMINO_LLM_URL")
        .ok()
        .or_else(|| crate::setting("llm_url"))
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEV_URL.to_owned());
    (url != "off").then(|| url.trim_end_matches('/').to_owned())
}

pub fn enabled() -> bool {
    base_url().is_some()
}

fn model(base: &str) -> Result<String, String> {
    if let Some(model) = crate::setting("llm_model").filter(|model| !model.is_empty()) {
        return Ok(model);
    }
    let mut cached = MODEL.lock().unwrap();
    if let Some(model) = cached.as_ref() {
        return Ok(model.clone());
    }
    let list = json::parse(&curl(&format!("{base}/models"), None, 5)?).ok_or("unreadable model list")?;
    let first = list.get("data").and_then(|data| data.array().first()).and_then(|model| model.str_at("id"));
    let model = first.ok_or("the server lists no models")?.to_owned();
    *cached = Some(model.clone());
    Ok(model)
}

/// The model's answer to `prompt` under `system`, trimmed.
pub fn complete(system: &str, prompt: &str, max_tokens: u32) -> Result<String, String> {
    let base = base_url().ok_or("no model server")?;
    let model = model(&base)?;
    // Thinking models (Gemma 4, Qwen 3) otherwise spend the whole budget
    // reasoning and answer nothing; llama.cpp's templates take this switch.
    let body = format!(
        r#"{{"model":{},"temperature":0.2,"chat_template_kwargs":{{"enable_thinking":false}},"max_tokens":{max_tokens},"messages":[{{"role":"system","content":{}}},{{"role":"user","content":{}}}]}}"#,
        quote(&model),
        quote(system),
        quote(prompt),
    );
    let reply = json::parse(&curl(&format!("{base}/chat/completions"), Some(&body), 60)?).ok_or("unreadable reply")?;
    let message = reply.get("choices").and_then(|c| c.array().first()).and_then(|c| c.get("message"));
    let text = message.and_then(|m| m.str_at("content")).ok_or("no answer in the reply")?;
    // Some templates still lead with an empty thought.
    let text = ["</think>", "<channel|>"].iter().fold(text, |text, end| text.rsplit(end).next().unwrap_or(text));
    Ok(text.trim().to_owned())
}

fn curl(url: &str, body: Option<&str>, seconds: u32) -> Result<String, String> {
    let mut command = Command::new("curl");
    command.args(["-fsS", "--max-time", &seconds.to_string(), url]);
    if body.is_some() {
        command.args(["-H", "Content-Type: application/json", "--data-binary", "@-"]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let (Some(body), Some(mut stdin)) = (body, child.stdin.take()) {
        use std::io::Write;
        let _ = stdin.write_all(body.as_bytes());
    }
    let output = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `text` as a JSON string.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::Value;

    #[test]
    fn quoted_text_reads_back() {
        let text = "say \"hi\"\n\tC:\\ \u{1b}[31m é";
        assert_eq!(json::parse(&quote(text)), Some(Value::String(text.to_owned())));
    }
}

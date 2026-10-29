//! A language model for the few things that want one (the tab recap's
//! summary), over any OpenAI-compatible server. Calls block: run them off
//! the UI thread. curl does the HTTP, as for updates.

use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::json;

/// The development model server, until there's a chosen one: the Orin on
/// the local network, else over the tailnet. Off both, every call just
/// fails and nothing is shown.
const DEV_URLS: [&str; 2] = ["http://orin.local:8080/v1", "http://jetson-orin.heron-minor.ts.net:8080/v1"];

/// The server that answered and its model, once asked: the first model it
/// lists, unless one is set.
static SERVER: Mutex<Option<(String, String)>> = Mutex::new(None);

/// The server's base URL as set: `TEMINO_LLM_URL`, then the `llm_url`
/// setting; None for the development server. `off` turns the model off.
fn configured() -> Option<Option<String>> {
    let url = std::env::var("TEMINO_LLM_URL").ok().or_else(|| crate::setting("llm_url")).filter(|url| !url.is_empty());
    match url {
        Some(url) if url == "off" => None,
        Some(url) => Some(Some(url.trim_end_matches('/').to_owned())),
        None => Some(None),
    }
}

/// Asks for the server and its model again: after its address or key
/// changed, or a call to it failed.
pub fn forget_model() {
    *SERVER.lock().unwrap() = None;
}

pub fn enabled() -> bool {
    configured().is_some()
}

/// The base URL to call and the model to ask for: the configured server,
/// or the first development server that answers.
fn server() -> Result<(String, String), String> {
    let urls = match configured().ok_or("no model server")? {
        Some(url) => vec![url],
        None => DEV_URLS.map(str::to_owned).to_vec(),
    };
    let mut cached = SERVER.lock().unwrap();
    if let Some((base, model)) = cached.as_ref().filter(|(base, _)| urls.contains(base)) {
        return Ok((base.clone(), model.clone()));
    }
    let mut error = String::new();
    for base in urls {
        match model(&base) {
            Ok(model) => {
                *cached = Some((base.clone(), model.clone()));
                return Ok((base, model));
            }
            Err(e) => error = e,
        }
    }
    Err(error)
}

fn model(base: &str) -> Result<String, String> {
    let listed = curl(&format!("{base}/models"), None, 3)?;
    if let Some(model) = crate::setting("llm_model").filter(|model| !model.is_empty()) {
        return Ok(model);
    }
    let list = json::parse(&listed).ok_or("unreadable model list")?;
    let first = list.get("data").and_then(|data| data.array().first()).and_then(|model| model.str_at("id"));
    Ok(first.ok_or("the server lists no models")?.to_owned())
}

/// The model's answer to `prompt` under `system`, trimmed.
pub fn complete(system: &str, prompt: &str, max_tokens: u32) -> Result<String, String> {
    let (base, model) = server()?;
    // Thinking models (Gemma 4, Qwen 3) otherwise spend the whole budget
    // reasoning and answer nothing; llama.cpp's templates take this switch.
    let body = format!(
        r#"{{"model":{},"temperature":0.2,"chat_template_kwargs":{{"enable_thinking":false}},"max_tokens":{max_tokens},"messages":[{{"role":"system","content":{}}},{{"role":"user","content":{}}}]}}"#,
        quote(&model),
        quote(system),
        quote(prompt),
    );
    let reply = curl(&format!("{base}/chat/completions"), Some(&body), 60).inspect_err(|_| forget_model())?;
    let reply = json::parse(&reply).ok_or("unreadable reply")?;
    let message = reply.get("choices").and_then(|c| c.array().first()).and_then(|c| c.get("message"));
    let text = message.and_then(|m| m.str_at("content")).ok_or("no answer in the reply")?;
    // Some templates still lead with an empty thought.
    let text = ["</think>", "<channel|>"].iter().fold(text, |text, end| text.rsplit(end).next().unwrap_or(text));
    Ok(text.trim().to_owned())
}

/// POSTs JSON `body` to `url` with a bearer `key`.
pub fn post_with_key(url: &str, key: &str, body: &str, seconds: u32) -> Result<String, String> {
    request(url, Some(body), Some(key), seconds)
}

/// The model server's key (the `llm_key` setting), if it wants one.
fn server_key() -> Option<String> {
    crate::setting("llm_key").filter(|key| !key.is_empty())
}

fn curl(url: &str, body: Option<&str>, seconds: u32) -> Result<String, String> {
    request(url, body, server_key().as_deref(), seconds)
}

/// GETs `url`, or POSTs JSON `body` to it, with a bearer `key` if given.
/// Everything goes to curl as a config on its stdin, so a key never shows
/// in the process list.
fn request(url: &str, body: Option<&str>, key: Option<&str>, seconds: u32) -> Result<String, String> {
    let quoted = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
    let mut config = format!("url = {}\n", quoted(url));
    if let Some(key) = key {
        config.push_str(&format!("header = {}\n", quoted(&format!("Authorization: Bearer {key}"))));
    }
    if let Some(body) = body {
        config.push_str("header = \"Content-Type: application/json\"\n");
        config.push_str(&format!("data-binary = {}\n", quoted(body)));
    }
    let mut child = Command::new("curl")
        .args(["-fsS", "--max-time", &seconds.to_string(), "-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(config.as_bytes());
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

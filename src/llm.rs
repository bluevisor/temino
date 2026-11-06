//! A language model for the few things that want one (the tab recap's
//! summary), over any OpenAI-compatible server. Calls block: run them off
//! the UI thread. curl does the HTTP, as for updates.

use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::json;

/// Where the model comes from: the `llm_provider` setting, picked in
/// Settings. Hosted ones list hundreds of models, so each names a quick,
/// cheap one rather than taking the first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    /// Empty for Off and Custom (`llm_url`).
    url: &'static str,
    /// Empty: the first model the server lists.
    pub model: &'static str,
    /// Where its key is kept, and the environment variable read without one.
    key_setting: &'static str,
    key_env: &'static str,
}

const fn provider(id: &'static str, name: &'static str, url: &'static str, model: &'static str) -> Provider {
    Provider { id, name, url, model, key_setting: "", key_env: "" }
}

pub const PROVIDERS: [Provider; 5] = [
    provider("off", "Off", "", ""),
    Provider {
        key_setting: "llm_key_vercel",
        key_env: "AI_GATEWAY_API_KEY",
        ..provider("vercel", "Vercel", "https://ai-gateway.vercel.sh/v1", "anthropic/claude-haiku-5.5")
    },
    Provider {
        key_setting: "llm_key_openai",
        key_env: "OPENAI_API_KEY",
        ..provider("openai", "OpenAI", "https://api.openai.com/v1", "gpt-4.1-mini")
    },
    Provider {
        key_setting: "llm_key_openrouter",
        key_env: "OPENROUTER_API_KEY",
        ..provider("openrouter", "OpenRouter", "https://openrouter.ai/api/v1", "anthropic/claude-haiku-5.5")
    },
    Provider { key_setting: "llm_key", ..provider("custom", "Custom", "", "") },
];

impl Provider {
    pub fn is(self, id: &str) -> bool {
        self.id == id
    }

    /// Whether it asks for a key: every hosted one, and a custom server may.
    pub fn takes_key(self) -> bool {
        !self.key_setting.is_empty()
    }

    /// The setting its model is kept under: hosted ones each their own,
    /// since one's model names mean nothing to another.
    pub fn model_setting(self) -> String {
        if self.url.is_empty() {
            "llm_model".to_owned()
        } else {
            format!("llm_model_{}", self.id)
        }
    }

    /// The setting its key is kept under.
    pub fn key_setting(self) -> &'static str {
        if self.takes_key() {
            self.key_setting
        } else {
            "llm_key"
        }
    }
}

/// The provider in use: the `llm_provider` setting; before there was one,
/// what `llm_url` says (empty or `off`: none, else a custom server).
pub fn chosen() -> Provider {
    let by_id = |id: &str| PROVIDERS.iter().copied().find(|p| p.is(id));
    if let Some(provider) = crate::setting("llm_provider").as_deref().and_then(by_id) {
        return provider;
    }
    match crate::setting("llm_url").unwrap_or_default().as_str() {
        "" | "off" => by_id("off"),
        _ => by_id("custom"),
    }
    .unwrap()
}

/// The server that answered and its model, once asked: the first model it
/// lists, unless one is set.
static SERVER: Mutex<Option<(String, String)>> = Mutex::new(None);

/// The servers' base URLs, the first that answers wins: `TEMINO_LLM_URL`
/// (`off` for none), else the provider's, or for Custom the `llm_url`
/// setting, which may list several (one on the LAN, then over a VPN). None
/// when off.
fn configured() -> Option<Vec<String>> {
    let urls = match std::env::var("TEMINO_LLM_URL").ok().filter(|url| !url.is_empty()) {
        Some(url) => url,
        None => match chosen() {
            provider if provider.is("custom") => crate::setting("llm_url").unwrap_or_default(),
            provider => provider.url.to_owned(),
        },
    };
    let urls: Vec<String> = urls
        .split([',', ' '])
        .filter(|url| !url.is_empty() && *url != "off")
        .map(|url| url.trim_end_matches('/').to_owned())
        .collect();
    (!urls.is_empty()).then_some(urls)
}

/// Asks for the server and its model again: after its address or key
/// changed, or a call to it failed.
pub fn forget_model() {
    *SERVER.lock().unwrap() = None;
}

pub fn enabled() -> bool {
    configured().is_some()
}

/// The base URL to call and the model to ask for: the first configured
/// server that answers.
fn server() -> Result<(String, String), String> {
    let urls = configured().ok_or("no model server")?;
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
    let provider = chosen();
    if let Some(model) = crate::setting(&provider.model_setting()).filter(|model| !model.is_empty()) {
        return Ok(model);
    }
    if !provider.model.is_empty() && base == provider.url {
        return Ok(provider.model.to_owned());
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
    // Hosted APIs pass it on to models that may refuse it, or refuse it.
    let hosted = PROVIDERS.iter().any(|p| !p.url.is_empty() && p.url == base);
    let thinking = if hosted { "" } else { r#""chat_template_kwargs":{"enable_thinking":false},"# };
    let body = format!(
        r#"{{"model":{},"temperature":0.2,{thinking}"max_tokens":{max_tokens},"messages":[{{"role":"system","content":{}}},{{"role":"user","content":{}}}]}}"#,
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

/// The provider's key, if it wants one: its own setting, else its usual
/// environment variable.
fn server_key() -> Option<String> {
    let provider = chosen();
    let set = crate::setting(provider.key_setting()).filter(|key| !key.is_empty());
    set.or_else(|| std::env::var(provider.key_env).ok().filter(|_| !provider.key_env.is_empty()))
        .filter(|key| !key.is_empty())
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
    fn providers_keep_their_keys_apart() {
        let keys: Vec<_> = PROVIDERS.iter().filter(|p| p.takes_key()).map(|p| p.key_setting()).collect();
        assert_eq!(keys, ["llm_key_vercel", "llm_key_openai", "llm_key_openrouter", "llm_key"]);
        // Hosted ones have a server and a model; Custom brings its own.
        for p in PROVIDERS.iter().filter(|p| p.takes_key() && !p.is("custom")) {
            assert!(p.url.starts_with("https://") && !p.model.is_empty(), "{}", p.id);
        }
    }

    #[test]
    fn quoted_text_reads_back() {
        let text = "say \"hi\"\n\tC:\\ \u{1b}[31m é";
        assert_eq!(json::parse(&quote(text)), Some(Value::String(text.to_owned())));
    }
}

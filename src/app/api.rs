//! The API socket's requests (api.rs), answered for this window's panes.

use super::*;

impl App {
    /// A request from the API socket (api.rs): its result, as JSON.
    pub(super) fn api(&mut self, method: &str, params: &crate::json::Value) -> Result<String, String> {
        use crate::remote::quote;
        let no = platform::window_no();
        let pane_param = || params.num_at("pane").map(|p| p as u64).ok_or("which pane? (\"pane\")".to_owned());
        let pane_at = |app: &App, id: u64| -> Result<usize, String> {
            app.panes.iter().position(|p| p.id == id).ok_or_else(|| format!("there's no pane {id} in window {no}"))
        };
        match method {
            "list" => {
                let focus = self.focused_id();
                let tabs: Vec<String> = (0..self.tabs.len())
                    .map(|index| {
                        let mut leaves = Vec::new();
                        self.tabs[index].root.leaves(&mut leaves);
                        let panes: Vec<String> = leaves
                            .iter()
                            .filter_map(|id| self.panes.iter().find(|p| p.id == *id))
                            .map(|pane| {
                                let running = pane.session.active().map(|b| b.command.clone());
                                format!(
                                    r#"{{"pane":{},"title":{},"cwd":{},"focused":{},"running":{},"agent":{},"program":{}}}"#,
                                    pane.id,
                                    quote(&pane.title()),
                                    quote(&pane.session.cwd),
                                    Some(pane.id) == focus,
                                    running.map_or("null".to_owned(), |c| quote(&c)),
                                    pane.agent_now.map_or("null".to_owned(), |s| quote(&format!("{s:?}").to_lowercase())),
                                    pane.program_now.as_ref().map_or("null".to_owned(), |(p, _)| quote(p)),
                                )
                            })
                            .collect();
                        format!(
                            r#"{{"index":{index},"title":{},"active":{},"panes":[{}]}}"#,
                            quote(&self.tab_title(index)),
                            index == self.active,
                            panes.join(",")
                        )
                    })
                    .collect();
                Ok(format!(r#"{{"window":{no},"tabs":[{}]}}"#, tabs.join(",")))
            }
            "new_tab" => {
                self.new_tab_in(params.str_at("cwd").map(str::to_owned))?;
                let id = self.focused_id().ok_or("the new tab has no pane")?;
                if let Some(command) = params.str_at("command") {
                    let index = pane_at(self, id)?;
                    self.panes[index].run_when_ready(command.to_owned());
                }
                Ok(format!(r#"{{"window":{no},"pane":{id}}}"#))
            }
            "split" => {
                if let Some(id) = params.num_at("pane") {
                    self.api_focus(id as u64)?;
                }
                let before = self.next_id;
                let axis = if params.str_at("direction") == Some("down") { Axis::Column } else { Axis::Row };
                self.split(axis, false);
                let id = self.focused_id().filter(|&id| id >= before).ok_or("couldn't split it")?;
                if let Some(command) = params.str_at("command") {
                    let index = pane_at(self, id)?;
                    self.panes[index].run_when_ready(command.to_owned());
                }
                Ok(format!(r#"{{"window":{no},"pane":{id}}}"#))
            }
            "focus" => {
                self.api_focus(pane_param()?)?;
                Ok("true".to_owned())
            }
            "close" => {
                let id = pane_param()?;
                pane_at(self, id)?;
                self.close_pane(id);
                Ok("true".to_owned())
            }
            "send_text" => {
                let index = pane_at(self, pane_param()?)?;
                let text = params.str_at("text").ok_or("what text? (\"text\")")?;
                self.panes[index].text_input(text);
                Ok("true".to_owned())
            }
            "send_keys" => {
                let index = pane_at(self, pane_param()?)?;
                let keys = params.get("keys").map(crate::json::Value::array).unwrap_or_default();
                if keys.is_empty() {
                    return Err("which keys? (\"keys\": [\"Enter\", \"ctrl+c\"])".into());
                }
                let mut events = Vec::new();
                for name in keys {
                    let name = name.str().ok_or("each key is a string")?;
                    events.push(api_key(name).ok_or_else(|| format!("{name} isn't a key Temino knows"))?);
                }
                let Self { panes, ctx, .. } = self;
                for event in events {
                    panes[index].key(ctx, event);
                }
                Ok("true".to_owned())
            }
            "run" => {
                let index = pane_at(self, pane_param()?)?;
                let command = params.str_at("command").filter(|c| !c.trim().is_empty()).ok_or("what command?")?;
                let pane = &mut self.panes[index];
                if pane.session.phase != crate::session::Phase::Ready {
                    return Err("that pane is busy: a command is running, or its shell isn't at its prompt".into());
                }
                pane.session.submit(command);
                let block = pane.session.blocks.last().map(|b| b.id).ok_or("no block came of it")?;
                Ok(format!(r#"{{"window":{no},"pane":{},"block":{block}}}"#, pane.id))
            }
            "block" | "read" => {
                let index = pane_at(self, pane_param()?)?;
                let lines = params.num_at("lines").map_or(200, |n| n.max(1.0) as usize);
                let pane = &mut self.panes[index];
                let mut describe = |block: &mut crate::session::Block| {
                    let output = block.output_text();
                    // The end of a long output: what was said last.
                    let start = output.lines().count().saturating_sub(lines);
                    let output: Vec<&str> = output.lines().skip(start).collect();
                    format!(
                        r#"{{"block":{},"command":{},"cwd":{},"running":{},"exit":{},"output":{}}}"#,
                        block.id,
                        quote(&block.command),
                        quote(&block.cwd),
                        block.running(),
                        block.exit.map_or("null".to_owned(), |e| e.to_string()),
                        quote(&output.join("\n")),
                    )
                };
                if method == "block" {
                    let id = params.num_at("block").ok_or("which block?")? as u64;
                    let block = pane.session.blocks.iter_mut().find(|b| b.id == id).ok_or("that block is gone")?;
                    return Ok(describe(block));
                }
                let count = params.num_at("blocks").map_or(1, |n| n.max(1.0) as usize);
                let commands = pane.session.blocks.iter_mut().filter(|b| b.kind == crate::session::BlockKind::Command);
                let mut picked: Vec<&mut crate::session::Block> = commands.collect();
                let skip = picked.len().saturating_sub(count);
                let described: Vec<String> = picked.drain(skip..).map(&mut describe).collect();
                Ok(format!(
                    r#"{{"window":{no},"pane":{},"cwd":{},"blocks":[{}]}}"#,
                    pane.id,
                    quote(&pane.session.cwd),
                    described.join(",")
                ))
            }
            other => Err(format!("{other} isn't a method Temino knows")),
        }
    }

    /// Brings pane `id`'s tab forward and gives the pane the keyboard.
    fn api_focus(&mut self, id: u64) -> Result<(), String> {
        let tab = self.tab_index_of(id).ok_or_else(|| format!("there's no pane {id} in this window"))?;
        if tab != self.active {
            self.switch_tab(tab);
        }
        if self.focused_id() != Some(id) {
            self.set_focus(id);
        }
        Ok(())
    }
}

/// A key named the API's way: "Enter", "ctrl+c", "shift+tab", "alt+x".
fn api_key(name: &str) -> Option<KeyEvent> {
    let mut parts: Vec<&str> = name.split('+').collect();
    let key = parts.pop().filter(|k| !k.is_empty()).or(name.ends_with('+').then_some("+"))?;
    let known = ["shift", "ctrl", "control", "alt", "option", "cmd", "command", "super", "meta"];
    if !parts.iter().all(|m| known.contains(&m.to_lowercase().as_str())) {
        return None;
    }
    let mods: Vec<crate::json::Value> = parts
        .iter()
        .map(|m| {
            let m = match m.to_lowercase().as_str() {
                "control" => "ctrl",
                "option" => "alt",
                "command" | "super" | "meta" => "cmd",
                other => return crate::json::Value::String(other.to_owned()),
            };
            crate::json::Value::String(m.to_owned())
        })
        .collect();
    let named = |key: &str| -> String {
        // The phone's names: Enter, Tab, Escape, Up, F5…
        let lower = key.to_lowercase();
        match lower.as_str() {
            "return" | "enter" => "Enter".into(),
            "esc" | "escape" => "Escape".into(),
            "tab" => "Tab".into(),
            "backspace" => "Backspace".into(),
            "delete" | "del" => "Delete".into(),
            "space" => " ".into(),
            "up" | "down" | "left" | "right" | "home" | "end" | "pageup" | "pagedown" | "insert" => {
                let mut c = lower.chars();
                let first = c.next().unwrap().to_uppercase().collect::<String>();
                let rest: String = c.collect();
                (first + &rest).replace("Pageup", "PageUp").replace("Pagedown", "PageDown")
            }
            _ if lower.len() > 1 && lower.starts_with('f') && lower[1..].parse::<u8>().is_ok() => lower.to_uppercase(),
            _ => key.to_owned(),
        }
    };
    let message = crate::json::Value::Object(vec![
        ("key".into(), crate::json::Value::String(named(key))),
        ("mods".into(), crate::json::Value::Array(mods)),
    ]);
    remote::key(&message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_by_name() {
        let ctrl_c = api_key("ctrl+c").unwrap();
        assert_eq!((ctrl_c.ctrl(), ctrl_c.raw_char), (true, 'c' as u32));
        assert_eq!(api_key("Enter").unwrap().named, Some(Named::Enter));
        assert_eq!(api_key("shift+Tab").unwrap().named, Some(Named::Tab));
        assert_eq!(api_key("F5").unwrap().named, Some(Named::F(5)));
        assert_eq!(api_key("pagedown").unwrap().named, Some(Named::PageDown));
        assert!(api_key("super+k").unwrap().cmd());
        assert!(api_key("hyper+z").is_none());
        assert!(api_key("F13").is_none());
    }
}

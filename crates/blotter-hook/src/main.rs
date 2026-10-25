//! blotter-hook: forward Claude Code hook events to blotterd.
//!
//! Runs as an async command hook for every event blotter tracks. It reads the hook's JSON on
//! stdin, keeps only what the board shows (never file contents or tool output), adds what the
//! JSON doesn't carry (tmux pane, host, Remote Control), and posts it to blotterd.
//!
//! It always exits 0 and gives up after a few seconds, so a missing config or an unreachable
//! Pi never gets in Claude's way. Problems go to stderr, which Claude Code shows in
//! `claude --debug`.
//!
//! Config: ~/.config/blotter/hook.toml
//!   url = "http://100.81.63.71:7340"
//!   token = "..."
//!   send_text = true      # false: no prompts or replies (tools, files and commands still go)
//!   host = "macbook"      # default: the short hostname

use blotter_proto::{now_ms, xdg_dir, Event, Tool};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const PROMPT_LIMIT: usize = 600;
const MESSAGE_LIMIT: usize = 1500;
const DETAIL_LIMIT: usize = 200;

#[derive(Deserialize)]
struct HookConfig {
    url: String,
    token: String,
    #[serde(default = "yes")]
    send_text: bool,
    host: Option<String>,
}

fn yes() -> bool {
    true
}

fn main() {
    if let Err(e) = run() {
        eprintln!("blotter-hook: {}", e);
    }
}

fn run() -> Result<(), String> {
    let path = xdg_dir("XDG_CONFIG_HOME", ".config").join("blotter").join("hook.toml");
    let config: HookConfig = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {}", path.display(), e))?,
        // Not set up on this machine: stay silent.
        Err(_) => return Ok(()),
    };

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).map_err(|e| e.to_string())?;
    let hook: Value = serde_json::from_str(&input).map_err(|e| format!("hook input: {}", e))?;
    let event = filter(&hook, &config);
    if event.session_id.is_empty() || event.hook.is_empty() {
        return Ok(());
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(4))
        .build();
    agent
        .post(&format!("{}/events", config.url.trim_end_matches('/')))
        .set("Authorization", &format!("Bearer {}", config.token))
        .send_json(&event)
        .map_err(|e| format!("posting to {}: {}", config.url, e))?;
    Ok(())
}

/// Turn the hook's input into the event blotterd stores.
fn filter(hook: &Value, config: &HookConfig) -> Event {
    let s = |key: &str| hook.get(key).and_then(Value::as_str).map(str::to_owned);
    let cwd = s("cwd").unwrap_or_default();
    let name = s("hook_event_name").unwrap_or_default();
    let text = |v: Option<String>, limit| if config.send_text { v.map(|t| clip(&t, limit)) } else { None };

    let tool = s("tool_name").map(|name| Tool {
        detail: hook.get("tool_input").and_then(|input| tool_detail(&name, input, &cwd)),
        name: short_tool_name(&name),
    });
    // StopFailure reports its error in last_assistant_message; it isn't a reply.
    let (message, last_message) = match name.as_str() {
        "StopFailure" => (s("error").or(s("last_assistant_message")), None),
        _ => (s("message"), s("last_assistant_message")),
    };

    Event {
        session_id: s("session_id").unwrap_or_default(),
        at: now_ms(),
        host: config.host.clone().unwrap_or_else(hostname),
        project: project_name(&cwd),
        tmux_pane: std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()),
        remote_control: std::env::var_os("CLAUDE_CODE_BRIDGE_SESSION_ID").is_some(),
        title: s("session_title"),
        notification_type: s("notification_type"),
        message: message.map(|m| clip(&m, DETAIL_LIMIT)),
        tool,
        prompt: text(s("prompt"), PROMPT_LIMIT),
        last_message: text(last_message, MESSAGE_LIMIT),
        hook: name,
        cwd,
    }
}

/// The one line that says what a tool call is doing: the file, command, pattern or URL.
fn tool_detail(tool: &str, input: &Value, cwd: &str) -> Option<String> {
    let field = |k: &str| input.get(k).and_then(Value::as_str);
    let detail = match tool {
        "Bash" => field("command").map(|c| c.lines().next().unwrap_or("").to_owned()),
        "Agent" | "Task" => field("description").map(str::to_owned),
        _ => ["file_path", "notebook_path", "path", "pattern", "url", "query", "description"]
            .iter()
            .find_map(|k| field(k))
            .map(|v| relative(v, cwd)),
    }?;
    Some(clip(&detail, DETAIL_LIMIT))
}

/// `mcp__github__create_issue` reads better as `github create_issue`.
fn short_tool_name(name: &str) -> String {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.replacen("__", " ", 1),
        None => name.to_owned(),
    }
}

fn relative(path: &str, cwd: &str) -> String {
    Path::new(path)
        .strip_prefix(cwd)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_owned())
}

/// The repository's name, shared by its worktrees, or the folder's name outside git.
fn project_name(cwd: &str) -> String {
    let common = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let root = match &common {
        Some(c) if c.ends_with("/.git") => Path::new(c).parent().unwrap_or(Path::new(c)),
        Some(c) => Path::new(c),
        None => Path::new(cwd),
    };
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| cwd.to_owned())
}

fn hostname() -> String {
    Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Keep at most `limit` characters, on a character boundary.
fn clip(text: &str, limit: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(limit) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(send_text: bool) -> HookConfig {
        HookConfig { url: String::new(), token: String::new(), send_text, host: Some("mac".into()) }
    }

    #[test]
    fn keeps_the_file_not_the_content() {
        let hook = json!({
            "session_id": "s1", "hook_event_name": "PreToolUse", "cwd": "/tmp/proj",
            "tool_name": "Write",
            "tool_input": {"file_path": "/tmp/proj/src/main.rs", "content": "fn main() {}"}
        });
        let ev = filter(&hook, &config(true));
        assert_eq!(ev.tool, Some(Tool { name: "Write".into(), detail: Some("src/main.rs".into()) }));
        assert!(!serde_json::to_string(&ev).unwrap().contains("fn main"));
    }

    #[test]
    fn bash_shows_the_first_line() {
        let hook = json!({"tool_name": "Bash", "tool_input": {"command": "cargo test\necho done"}});
        assert_eq!(filter(&hook, &config(true)).tool.unwrap().detail.as_deref(), Some("cargo test"));
    }

    #[test]
    fn send_text_off_drops_prompts_and_replies() {
        let hook = json!({"hook_event_name": "Stop", "prompt": "secret", "last_assistant_message": "reply"});
        let ev = filter(&hook, &config(false));
        assert_eq!(ev.prompt, None);
        assert_eq!(ev.last_message, None);
    }

    #[test]
    fn stop_failure_error_is_not_a_reply() {
        let hook = json!({"hook_event_name": "StopFailure", "error": "rate_limit", "last_assistant_message": "API Error: 429"});
        let ev = filter(&hook, &config(true));
        assert_eq!(ev.message.as_deref(), Some("rate_limit"));
        assert_eq!(ev.last_message, None);
    }

    #[test]
    fn clips_on_char_boundaries() {
        assert_eq!(clip("héllo", 2), "hé…");
        assert_eq!(clip("hi", 5), "hi");
    }

    #[test]
    fn shortens_mcp_tool_names() {
        assert_eq!(short_tool_name("mcp__github__create_issue"), "github create_issue");
        assert_eq!(short_tool_name("Edit"), "Edit");
    }
}

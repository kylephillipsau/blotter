//! Types shared by every part of blotter: the event `blotter-hook` sends from the Mac, the board
//! `blotterd` keeps, and the config file `blotterd` and `blotter` read.
//!
//! Session states follow sofle-status, so the keyboard and the Classic always agree:
//!
//!   SessionStart                          idle
//!   UserPromptSubmit, Pre/PostToolUse     working
//!   PermissionRequest, permission prompt  needs you
//!   Stop, StopFailure                     idle
//!   SessionEnd                            removed

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// One Claude Code hook event, filtered on the Mac before it is sent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Event {
    pub session_id: String,
    /// The hook's `hook_event_name`.
    pub hook: String,
    /// When the hook ran, in Unix milliseconds. Async hooks can arrive out of order; this puts
    /// them back in order.
    pub at: u64,
    pub host: String,
    pub cwd: String,
    /// The git repository's name, or the folder's when it isn't one.
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_pane: Option<String>,
    #[serde(default)]
    pub remote_control: bool,
    /// A custom session name (`--name`, `/rename`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_type: Option<String>,
    /// A notification's text, or a StopFailure's error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Claude's last reply, from Stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
}

/// A tool call, summarised: `Edit` and the file, `Bash` and the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl std::fmt::Display for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match &self.detail {
            Some(d) => write!(f, "{}: {}", self.name, d),
            None => f.write_str(&self.name),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Idle,
    Working,
    NeedsYou,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub host: String,
    pub cwd: String,
    pub project: String,
    pub tmux_pane: Option<String>,
    pub remote_control: bool,
    pub title: Option<String>,
    pub state: State,
    /// When the state last changed (Unix ms).
    pub since: u64,
    /// The newest event applied (Unix ms).
    pub updated: u64,
    /// The tool running now, or the last one.
    pub tool: Option<Tool>,
    /// What the session is waiting on you for.
    pub waiting_on: Option<String>,
    pub prompt: Option<String>,
    pub last_message: Option<String>,
    pub error: Option<String>,
}

impl Session {
    fn new(ev: &Event) -> Self {
        Session {
            id: ev.session_id.clone(),
            host: ev.host.clone(),
            cwd: ev.cwd.clone(),
            project: ev.project.clone(),
            tmux_pane: None,
            remote_control: false,
            title: None,
            state: State::Idle,
            since: ev.at,
            updated: 0,
            tool: None,
            waiting_on: None,
            prompt: None,
            last_message: None,
            error: None,
        }
    }

    fn set_state(&mut self, state: State, at: u64) {
        if self.state != state {
            self.state = state;
            self.since = at;
        }
    }

    /// Working or waiting, but silent for so long the session has probably died.
    pub fn is_stale(&self, now: u64, stale_after_ms: u64) -> bool {
        self.state != State::Idle && now.saturating_sub(self.updated) > stale_after_ms
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Board {
    /// Goes up by one with every visible change, so clients can wait for the next one.
    pub version: u64,
    pub sessions: BTreeMap<String, Session>,
}

impl Board {
    /// Apply one event. Returns whether the board changed.
    pub fn apply(&mut self, ev: &Event) -> bool {
        if ev.session_id.is_empty() {
            return false;
        }
        if ev.hook == "SessionEnd" {
            // A late event from before the end could otherwise bring the session back, so
            // only an end at least as new as the session's last event removes it.
            let ended = match self.sessions.get(&ev.session_id) {
                Some(s) => ev.at >= s.updated,
                None => false,
            };
            if ended {
                self.sessions.remove(&ev.session_id);
                self.version += 1;
            }
            return ended;
        }

        let s = self.sessions.entry(ev.session_id.clone()).or_insert_with(|| Session::new(ev));
        let before = s.clone();

        // Where the session runs can always be refreshed, even from a late event.
        s.host = ev.host.clone();
        s.cwd = ev.cwd.clone();
        s.project = ev.project.clone();
        if ev.tmux_pane.is_some() {
            s.tmux_pane = ev.tmux_pane.clone();
        }
        s.remote_control = ev.remote_control;
        if ev.title.is_some() {
            s.title = ev.title.clone();
        }

        // State only moves forward in time.
        if ev.at >= s.updated {
            s.updated = ev.at;
            match ev.hook.as_str() {
                "SessionStart" => {
                    s.set_state(State::Idle, ev.at);
                    s.tool = None;
                    s.waiting_on = None;
                    s.error = None;
                }
                "UserPromptSubmit" => {
                    s.set_state(State::Working, ev.at);
                    s.prompt = ev.prompt.clone();
                    s.tool = None;
                    s.waiting_on = None;
                    s.error = None;
                }
                "PreToolUse" | "PostToolUse" => {
                    s.set_state(State::Working, ev.at);
                    if ev.tool.is_some() {
                        s.tool = ev.tool.clone();
                    }
                    s.waiting_on = None;
                }
                "PermissionRequest" => {
                    s.set_state(State::NeedsYou, ev.at);
                    s.waiting_on = ev.tool.as_ref().map(|t| t.to_string()).or(ev.message.clone());
                }
                "Notification" => match ev.notification_type.as_deref() {
                    Some("permission_prompt") | Some("elicitation_dialog") | Some("agent_needs_input") => {
                        s.set_state(State::NeedsYou, ev.at);
                        // PermissionRequest usually got here first with the exact tool.
                        if s.waiting_on.is_none() {
                            s.waiting_on = ev.message.clone();
                        }
                    }
                    _ => {}
                },
                "Stop" => {
                    s.set_state(State::Idle, ev.at);
                    s.tool = None;
                    s.waiting_on = None;
                    if ev.last_message.is_some() {
                        s.last_message = ev.last_message.clone();
                    }
                }
                "StopFailure" => {
                    s.set_state(State::Idle, ev.at);
                    s.tool = None;
                    s.waiting_on = None;
                    s.error = ev.message.clone();
                }
                _ => {}
            }
        }

        let changed = *s != before;
        if changed {
            self.version += 1;
        }
        changed
    }

    /// Forget sessions with no events for `forget_after_ms`: ones that crashed or were killed
    /// without a SessionEnd. Returns whether any were removed.
    pub fn prune(&mut self, now: u64, forget_after_ms: u64) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|_, s| now.saturating_sub(s.updated) <= forget_after_ms);
        let changed = self.sessions.len() != before;
        if changed {
            self.version += 1;
        }
        changed
    }

    /// Sessions in display order: needing you first, then working, then idle; within each,
    /// the most recently changed first.
    pub fn sorted(&self) -> Vec<&Session> {
        let rank = |s: &Session| match s.state {
            State::NeedsYou => 0,
            State::Working => 1,
            State::Idle => 2,
        };
        let mut v: Vec<&Session> = self.sessions.values().collect();
        v.sort_by(|a, b| rank(a).cmp(&rank(b)).then(b.since.cmp(&a.since)));
        v
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Parse a duration like `90s`, `15m`, `24h` or `2d` into milliseconds.
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: u64 = n.parse().map_err(|_| format!("bad duration {:?}", s))?;
    let ms = match unit.trim() {
        "ms" => 1,
        "s" | "" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return Err(format!("bad duration unit in {:?} (use s, m, h or d)", s)),
    };
    Ok(n * ms)
}

/// The config file `blotterd` and `blotter` share. Sections other parts of blotter will use
/// later (`[mode]`, `[work]`, `[mac]`, `[keys]`) are ignored for now.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub feed: Feed,
    pub panels: Panels,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Feed {
    /// Address blotterd listens on and blotter connects to.
    pub listen: String,
    /// Bearer token every request must carry.
    pub token: String,
    /// Where blotterd keeps the board between restarts.
    /// Default: $XDG_STATE_HOME/blotter/board.json
    pub state_file: Option<PathBuf>,
    /// Forget a session after this long without events.
    pub forget_after: String,
}

impl Default for Feed {
    fn default() -> Self {
        Feed { listen: "127.0.0.1:7340".into(), token: String::new(), state_file: None, forget_after: "24h".into() }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Panels {
    pub agents: AgentsPanel,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AgentsPanel {
    /// Show a working or waiting session as stale after this long without events.
    pub stale_after: String,
}

impl Default for AgentsPanel {
    fn default() -> Self {
        AgentsPanel { stale_after: "30m".into() }
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        xdg_dir("XDG_CONFIG_HOME", ".config").join("blotter").join("config.toml")
    }

    pub fn load(path: &std::path::Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("reading {}: {}", path.display(), e))?;
        let config: Config = toml::from_str(&text).map_err(|e| format!("parsing {}: {}", path.display(), e))?;
        if config.feed.token.is_empty() {
            return Err(format!("{}: [feed] token is required", path.display()));
        }
        parse_duration(&config.feed.forget_after)?;
        parse_duration(&config.panels.agents.stale_after)?;
        Ok(config)
    }

    pub fn state_file(&self) -> PathBuf {
        self.feed
            .state_file
            .clone()
            .unwrap_or_else(|| xdg_dir("XDG_STATE_HOME", ".local/state").join("blotter").join("board.json"))
    }
}

pub fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(hook: &str, at: u64) -> Event {
        Event {
            session_id: "s1".into(),
            hook: hook.into(),
            at,
            host: "mac".into(),
            cwd: "/dev/nosdesk".into(),
            project: "Nosdesk".into(),
            ..Default::default()
        }
    }

    fn tool(name: &str, detail: &str) -> Option<Tool> {
        Some(Tool { name: name.into(), detail: Some(detail.into()) })
    }

    #[test]
    fn follows_a_session_through_a_turn() {
        let mut b = Board::default();
        assert!(b.apply(&ev("SessionStart", 1)));
        assert_eq!(b.sessions["s1"].state, State::Idle);

        assert!(b.apply(&Event { prompt: Some("fix the tests".into()), ..ev("UserPromptSubmit", 2) }));
        assert_eq!(b.sessions["s1"].state, State::Working);
        assert_eq!(b.sessions["s1"].prompt.as_deref(), Some("fix the tests"));

        b.apply(&Event { tool: tool("Bash", "cargo test"), ..ev("PermissionRequest", 3) });
        assert_eq!(b.sessions["s1"].state, State::NeedsYou);
        assert_eq!(b.sessions["s1"].waiting_on.as_deref(), Some("Bash: cargo test"));

        // The permission notification arrives too, but the exact tool stays.
        b.apply(&Event {
            notification_type: Some("permission_prompt".into()),
            message: Some("Claude needs your permission to use Bash".into()),
            ..ev("Notification", 4)
        });
        assert_eq!(b.sessions["s1"].waiting_on.as_deref(), Some("Bash: cargo test"));

        b.apply(&Event { tool: tool("Bash", "cargo test"), ..ev("PostToolUse", 5) });
        assert_eq!(b.sessions["s1"].state, State::Working);
        assert_eq!(b.sessions["s1"].waiting_on, None);

        b.apply(&Event { last_message: Some("All green.".into()), ..ev("Stop", 6) });
        let s = &b.sessions["s1"];
        assert_eq!(s.state, State::Idle);
        assert_eq!(s.since, 6);
        assert_eq!(s.last_message.as_deref(), Some("All green."));
        assert_eq!(s.prompt.as_deref(), Some("fix the tests"));

        assert!(b.apply(&ev("SessionEnd", 7)));
        assert!(b.sessions.is_empty());
    }

    #[test]
    fn late_events_dont_rewind_state() {
        let mut b = Board::default();
        b.apply(&ev("UserPromptSubmit", 10));
        b.apply(&ev("Stop", 30));
        // A PostToolUse that ran before Stop but arrived after it.
        b.apply(&Event { tool: tool("Edit", "src/lib.rs"), ..ev("PostToolUse", 20) });
        assert_eq!(b.sessions["s1"].state, State::Idle);

        // Nor does a late event bring an ended session back.
        b.apply(&ev("SessionEnd", 40));
        assert!(b.sessions.is_empty());
        b.apply(&ev("SessionEnd", 35));
        assert!(b.sessions.is_empty());
    }

    #[test]
    fn early_end_does_not_remove_a_newer_session() {
        let mut b = Board::default();
        b.apply(&ev("UserPromptSubmit", 50));
        assert!(!b.apply(&ev("SessionEnd", 40)));
        assert_eq!(b.sessions.len(), 1);
    }

    #[test]
    fn version_counts_only_changes() {
        let mut b = Board::default();
        b.apply(&ev("UserPromptSubmit", 1));
        let v = b.version;
        // Same state, same everything: no new version for clients to wake up for.
        assert!(!b.apply(&ev("PostToolUse", 1)));
        assert_eq!(b.version, v);
    }

    #[test]
    fn sorts_needs_you_first() {
        let mut b = Board::default();
        for (id, hook, at) in [("a", "Stop", 5), ("b", "UserPromptSubmit", 3), ("c", "PermissionRequest", 1)] {
            b.apply(&Event { session_id: id.into(), ..ev(hook, at) });
        }
        let order: Vec<&str> = b.sorted().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(order, ["c", "b", "a"]);
    }

    #[test]
    fn prunes_and_flags_stale_sessions() {
        let mut b = Board::default();
        b.apply(&ev("UserPromptSubmit", 1_000));
        assert!(b.sessions["s1"].is_stale(1_000 + 31 * 60_000, 30 * 60_000));
        assert!(!b.prune(2_000, 60_000));
        assert!(b.prune(100_000, 60_000));
        assert!(b.sessions.is_empty());
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("15m"), Ok(900_000));
        assert_eq!(parse_duration("24h"), Ok(86_400_000));
        assert_eq!(parse_duration("90"), Ok(90_000));
        assert!(parse_duration("soon").is_err());
    }
}

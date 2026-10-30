//! Agent session model, built from hook events and statusline data. Pure (no Win32).

use std::time::{Duration, Instant};

use notch_protocol::{HookEvent, Status};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    /// Waiting for the user's permission or answer.
    Waiting,
    Idle,
    Done,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub project: String,
    pub state: AgentState,
    /// What is being worked on right now, e.g. "Running cargo test" or "Thinking…".
    pub activity: String,
    /// Last completed action, shown dimmed above the activity.
    pub last: Option<String>,
    pub steps: u32,
    pub updated: Instant,
    /// When the last prompt was sent; for time spent working.
    pub started: Instant,
    /// Latest context tokens from the session transcript.
    pub context: Option<u64>,
    /// Snippet of the last prompt: what this session is working on.
    pub prompt: Option<String>,
    /// Completed actions, newest first (max. `HISTORY`).
    pub history: Vec<String>,
    /// Model ID from the transcript, e.g. "claude-opus-5-5".
    pub model: Option<String>,
    /// Project folder, if the bridge sent it.
    pub dir: Option<String>,
    /// Progress reported by the agent via MCP; shown in Live while the agent thinks between tools.
    pub progress: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub model: Option<String>,
    pub context_pct: Option<f32>,
    pub five_hour_pct: Option<f32>,
    pub seven_day_pct: Option<f32>,
    /// When the quota resets (Unix seconds).
    pub five_hour_resets: Option<u64>,
    pub seven_day_resets: Option<u64>,
    /// Codex quota (from its session logs), if "Codex usage" is on.
    pub codex: Vec<crate::codex_quota::Window>,
}

#[derive(Debug, PartialEq)]
pub enum Notice {
    /// `prompt` and `dir` for the chip's buttons: what is being worked on, and where.
    Finished { project: String, steps: u32, prompt: Option<String>, dir: Option<String> },
    /// The agent asked a question or wants plan approval, and is waiting for an answer in the agent app.
    NeedsYou { project: String, what: &'static str },
}

/// Tools that make the agent stop and wait for the user's answer, with the text to display.
fn asks_user(tool: &str) -> Option<&'static str> {
    match tool {
        "AskUserQuestion" => Some("Needs your answer"),
        "ExitPlanMode" => Some("Plan ready for your review"),
        _ => None,
    }
}

pub const THINKING: &str = "Thinking…";

/// Number of recent actions remembered per session, for the row expanded in the Agent tab.
pub const HISTORY: usize = 3;

/// A session with no news for this long is considered no longer working.
const STALE: Duration = Duration::from_secs(10 * 60);
/// Finished/idle sessions are dropped from the list after this.
const FORGET: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
pub struct Agents {
    sessions: Vec<Session>,
    pub usage: Option<Usage>,
}

/// Program of a shell command, for brief display: "cargo test", "git push".
/// Commands that are hard to read (variables, paths, quotes) simply become "a command".
fn short_command(cmd: &str) -> String {
    let mut words = cmd.split_whitespace();
    let readable = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    match (words.next(), words.next()) {
        (Some(a), Some(b)) if readable(a) && readable(b) && !b.starts_with('-') => format!("{a} {b}"),
        (Some(a), _) if readable(a) => a.to_string(),
        _ => "a command".into(),
    }
}

/// Activity sentence, e.g. "Editing auth.service.ts", "Running npm test". Shell commands
/// are shortened; the full command is shown only on the permission card.
pub fn describe(tool: &str, summary: Option<&str>) -> String {
    match tool {
        "AskUserQuestion" => return "Asked you a question".into(),
        "ExitPlanMode" => return "Shared a plan".into(),
        _ => {}
    }
    let verb = match tool {
        "Read" => "Reading",
        "Write" => "Writing",
        "Edit" | "MultiEdit" | "NotebookEdit" => "Editing",
        "Bash" | "PowerShell" => "Running",
        "Grep" | "Glob" => "Searching",
        "WebFetch" => "Opening",
        "WebSearch" => "Searching the web",
        "Task" | "Agent" => "Subagent",
        // MCP tool: "mcp__<server>__<tool>" becomes "Using <server>", e.g. "Using Claude Browser".
        _ => {
            return match tool.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
                Some((server, _)) => format!("Using {}", server.replace('_', " ")),
                None => summary.unwrap_or(tool).to_string(),
            }
        }
    };
    match summary {
        Some(s) if matches!(tool, "Bash" | "PowerShell") => format!("{verb} {}", short_command(s)),
        Some(s) if s != tool => format!("{verb} {s}"),
        _ => verb.to_string(),
    }
}

/// Readable model name from its ID: "claude-opus-5-5" → "Opus 5.5",
/// "claude-sonnet-4-5-20250929" → "Sonnet 4.5", "claude-3-5-haiku-20241022" → "Haiku 3.5".
pub fn model_name(id: &str) -> String {
    // Suffixes like "[1m]" (context window) are not part of the name.
    let id = id.split('[').next().unwrap_or(id);
    let words: Vec<&str> = id.trim_start_matches("claude-").split('-').collect();
    let family = words.iter().find(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphabetic()));
    // Release date (8 digits) is not part of the version.
    let version: Vec<&str> = words.iter().copied().filter(|w| !w.is_empty() && w.len() <= 2 && w.chars().all(|c| c.is_ascii_digit())).collect();
    match family {
        Some(f) => {
            let mut chars = f.chars();
            let mut name: String = chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
            if !version.is_empty() {
                name = format!("{name} {}", version.join("."));
            }
            name
        }
        None => id.to_string(),
    }
}

/// "1 step" / "3 steps".
pub fn steps_word(n: u32) -> &'static str {
    if n == 1 {
        "step"
    } else {
        "steps"
    }
}

impl Agents {
    fn session(&mut self, ev: &HookEvent, now: Instant) -> &mut Session {
        let i = match self.sessions.iter().position(|s| s.id == ev.session) {
            Some(i) => i,
            None => {
                self.sessions.push(Session {
                    id: ev.session.clone(),
                    project: ev.project.clone(),
                    state: AgentState::Idle,
                    activity: "Ready".into(),
                    last: None,
                    steps: 0,
                    updated: now,
                    started: now,
                    context: None,
                    model: None,
                    prompt: None,
                    history: Vec::new(),
                    dir: None,
                    progress: None,
                });
                self.sessions.len() - 1
            }
        };
        let s = &mut self.sessions[i];
        s.updated = now;
        if !ev.project.is_empty() {
            s.project = ev.project.clone();
        }
        if ev.cwd.is_some() {
            s.dir = ev.cwd.clone();
        }
        s
    }

    pub fn apply(&mut self, ev: &HookEvent, now: Instant) -> Option<Notice> {
        if ev.event == "SessionEnd" {
            self.sessions.retain(|s| s.id != ev.session);
            return None;
        }
        // Shell command: the agent's description is clearer than the command itself ("Running cd").
        let action = || ev.description.clone().unwrap_or_else(|| describe(ev.tool.as_deref().unwrap_or("?"), ev.summary.as_deref()));
        let s = self.session(ev, now);
        if ev.context_tokens.is_some() {
            s.context = ev.context_tokens;
        }
        if ev.model.is_some() {
            s.model = ev.model.clone();
        }
        match ev.event.as_str() {
            "SessionStart" => {
                s.state = AgentState::Idle;
                s.activity = "Ready".into();
            }
            "UserPromptSubmit" => {
                s.progress = None;
                s.state = AgentState::Working;
                s.activity = THINKING.into();
                s.last = None;
                s.steps = 0;
                s.started = now;
                s.history.clear();
                if ev.message.is_some() {
                    s.prompt = ev.message.clone();
                }
            }
            "PreToolUse" => match ev.tool.as_deref().and_then(asks_user) {
                // Questions and plan approvals are answered in the agent app; the notch marks them like a permission.
                Some(what) => {
                    s.state = AgentState::Waiting;
                    s.activity = what.into();
                    return Some(Notice::NeedsYou { project: s.project.clone(), what });
                }
                None => {
                    s.state = AgentState::Working;
                    s.activity = action();
                }
            },
            "PostToolUse" => {
                // Tool finished: move to the "last" row, then the model goes back to thinking.
                s.steps += 1;
                s.state = AgentState::Working;
                let done = action();
                s.history.insert(0, done.clone());
                s.history.truncate(HISTORY);
                s.last = Some(done);
                s.activity = THINKING.into();
            }
            "PermissionRequest" => {
                s.state = AgentState::Waiting;
                s.last = Some(action());
                s.activity = "Needs your approval".into();
            }
            "Notification" => match ev.notification_type.as_deref() {
                Some("permission_prompt") => s.state = AgentState::Waiting,
                Some("idle_prompt") => {
                    s.state = AgentState::Idle;
                    s.activity = "Waiting for your reply".into();
                }
                _ => {}
            },
            "Stop" => {
                s.progress = None;
                let was_busy = matches!(s.state, AgentState::Working | AgentState::Waiting);
                s.state = AgentState::Done;
                s.activity = format!("Finished · {} {}", s.steps, steps_word(s.steps));
                if was_busy {
                    return Some(Notice::Finished { project: s.project.clone(), steps: s.steps, prompt: s.prompt.clone(), dir: s.dir.clone() });
                }
            }
            _ => {}
        }
        None
    }

    /// MCP progress for the session that most recently worked. `false` if no session is working.
    pub fn progress(&mut self, text: &str, now: Instant) -> bool {
        let busy = self.sessions.iter_mut().filter(|s| matches!(s.state, AgentState::Working | AgentState::Waiting)).max_by_key(|s| s.updated);
        match busy {
            Some(s) => {
                s.progress = Some(text.to_string());
                s.updated = now;
                true
            }
            None => false,
        }
    }

    /// Agents that use the notch only via MCP (e.g. Chat in the Claude app, Codex) send no
    /// hook events; their latest tool call becomes one session row per agent.
    pub fn mcp_activity(&mut self, agent: &str, activity: String, state: AgentState, now: Instant) {
        let id = format!("mcp:{agent}");
        let i = match self.sessions.iter().position(|s| s.id == id) {
            Some(i) => i,
            None => {
                self.sessions.push(Session {
                    id,
                    project: agent.to_string(),
                    state: AgentState::Idle,
                    activity: String::new(),
                    last: None,
                    steps: 0,
                    updated: now,
                    started: now,
                    context: None,
                    model: None,
                    prompt: None,
                    history: Vec::new(),
                    dir: None,
                    progress: None,
                });
                self.sessions.len() - 1
            }
        };
        let s = &mut self.sessions[i];
        if state == AgentState::Working && s.state != AgentState::Working {
            s.started = now;
        }
        if !s.activity.is_empty() && s.activity != activity {
            s.history.insert(0, std::mem::take(&mut s.activity));
            s.history.truncate(HISTORY);
        }
        s.state = state;
        s.activity = activity;
        s.updated = now;
    }

    pub fn apply_status(&mut self, st: &Status) {
        // Quota is per account; use the latest value from whichever session reported it.
        let usage = self.usage.get_or_insert_with(Usage::default);
        usage.model = st.model.clone().or(usage.model.take());
        usage.context_pct = st.context_pct.or(usage.context_pct);
        usage.five_hour_pct = st.five_hour_pct.or(usage.five_hour_pct);
        usage.seven_day_pct = st.seven_day_pct.or(usage.seven_day_pct);
        usage.five_hour_resets = st.five_hour_resets_at.or(usage.five_hour_resets);
        usage.seven_day_resets = st.seven_day_resets_at.or(usage.seven_day_resets);
    }

    /// Official quota from Claude's usage endpoint (see `quota`); overrides the statusLine numbers.
    pub fn apply_quota(&mut self, q: &crate::quota::Quota) {
        let usage = self.usage.get_or_insert_with(Usage::default);
        if let Some(l) = q.five_hour {
            usage.five_hour_pct = Some(l.pct);
            usage.five_hour_resets = l.resets_at.or(usage.five_hour_resets);
        }
        if let Some(l) = q.seven_day {
            usage.seven_day_pct = Some(l.pct);
            usage.seven_day_resets = l.resets_at.or(usage.seven_day_resets);
        }
    }

    /// Tidies up old sessions; returns `true` if anything changed.
    pub fn prune(&mut self, now: Instant) -> bool {
        let before = self.sessions.len();
        let mut changed = false;
        for s in &mut self.sessions {
            if matches!(s.state, AgentState::Working | AgentState::Waiting) && now.duration_since(s.updated) > STALE {
                s.state = AgentState::Idle;
                s.activity = "No updates".into();
                changed = true;
            }
        }
        self.sessions.retain(|s| matches!(s.state, AgentState::Working | AgentState::Waiting) || now.duration_since(s.updated) < FORGET);
        changed || self.sessions.len() != before
    }

    /// Newest session first.
    pub fn sessions(&self) -> Vec<&Session> {
        let mut v: Vec<&Session> = self.sessions.iter().collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.updated));
        v
    }

    /// For the Agent tab list: those waiting for permission, then working ones, then the rest; each
    /// newest first.
    pub fn sessions_by_state(&self) -> Vec<&Session> {
        let rank = |s: &Session| match s.state {
            AgentState::Waiting => 0,
            AgentState::Working => 1,
            AgentState::Done => 2,
            AgentState::Idle => 3,
        };
        let mut v = self.sessions();
        v.sort_by_key(|s| rank(s));
        v
    }

    /// Context and model of the newest session that has reported them.
    pub fn latest_context(&self) -> Option<(u64, Option<&str>)> {
        self.sessions().into_iter().find_map(|s| s.context.map(|c| (c, s.model.as_deref())))
    }

    /// Session `id` if it is still working or waiting.
    pub fn active_by_id(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id && matches!(s.state, AgentState::Working | AgentState::Waiting))
    }

    /// The most recent session that is working or waiting.
    pub fn active(&self) -> Option<&Session> {
        self.sessions().into_iter().find(|s| matches!(s.state, AgentState::Working | AgentState::Waiting))
    }

    pub fn active_count(&self) -> usize {
        self.sessions.iter().filter(|s| matches!(s.state, AgentState::Working | AgentState::Waiting)).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: &str, tool: Option<&str>, summary: Option<&str>) -> HookEvent {
        HookEvent {
            source: "claude".into(),
            event: event.into(),
            session: "s1".into(),
            project: "notch".into(),
            tool: tool.map(Into::into),
            summary: summary.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn lifecycle_produces_activity_and_finish_notice() {
        let mut a = Agents::default();
        let t = Instant::now();
        a.apply(&ev("SessionStart", None, None), t);
        assert!(a.active().is_none());
        let prompt = HookEvent { message: Some("tidy up auth".into()), cwd: Some("C:\\code\\notch".into()), ..ev("UserPromptSubmit", None, None) };
        a.apply(&prompt, t);
        a.apply(&ev("PreToolUse", Some("Edit"), Some("auth.ts")), t);
        assert_eq!(a.active().unwrap().activity, "Editing auth.ts");
        a.apply(&ev("PostToolUse", Some("Edit"), Some("auth.ts")), t);
        let s = a.active().unwrap();
        assert_eq!((s.activity.as_str(), s.last.as_deref()), (THINKING, Some("Editing auth.ts")));
        a.apply(&ev("PermissionRequest", Some("Bash"), Some("npm test")), t);
        assert_eq!(a.active().unwrap().state, AgentState::Waiting);
        assert_eq!(a.active().unwrap().last.as_deref(), Some("Running npm test"));
        a.apply(&ev("PostToolUse", Some("Bash"), Some("npm test")), t);
        let notice = a.apply(&ev("Stop", None, None), t);
        let (prompt, dir) = (Some("tidy up auth".to_string()), Some("C:\\code\\notch".to_string()));
        assert_eq!(notice, Some(Notice::Finished { project: "notch".into(), steps: 2, prompt, dir }));
        assert!(a.active().is_none());
        // A second Stop (with no work in between) does not notify again.
        assert_eq!(a.apply(&ev("Stop", None, None), t), None);
        a.apply(&ev("SessionEnd", None, None), t);
        assert!(a.sessions().is_empty());
    }

    #[test]
    fn stale_sessions_go_idle_then_disappear() {
        let mut a = Agents::default();
        let t = Instant::now();
        a.apply(&ev("PreToolUse", Some("Bash"), Some("cargo build")), t);
        assert!(!a.prune(t + Duration::from_secs(60)));
        assert!(a.prune(t + STALE + Duration::from_secs(1)));
        assert_eq!(a.sessions()[0].state, AgentState::Idle);
        assert!(a.prune(t + FORGET + Duration::from_secs(1)));
        assert!(a.sessions().is_empty());
    }

    #[test]
    fn usage_keeps_last_known_values() {
        let mut a = Agents::default();
        a.apply_status(&Status { five_hour_pct: Some(20.0), context_pct: Some(5.0), ..Default::default() });
        a.apply_status(&Status { context_pct: Some(9.0), ..Default::default() });
        let u = a.usage.clone().unwrap();
        assert_eq!((u.five_hour_pct, u.context_pct), (Some(20.0), Some(9.0)));
    }

    #[test]
    fn context_follows_the_latest_report() {
        let mut a = Agents::default();
        let t = Instant::now();
        a.apply(&ev("UserPromptSubmit", None, None), t);
        assert_eq!(a.latest_context(), None);
        let mut with_ctx = ev("PostToolUse", Some("Read"), Some("a.rs"));
        with_ctx.context_tokens = Some(42_000);
        with_ctx.model = Some("claude-opus-5-5".into());
        a.apply(&with_ctx, t);
        // An event without numbers (e.g. PreToolUse) does not erase the last numbers.
        a.apply(&ev("PreToolUse", Some("Bash"), Some("ls")), t);
        assert_eq!(a.latest_context(), Some((42_000, Some("claude-opus-5-5"))));
    }

    #[test]
    fn questions_wait_for_the_user() {
        let mut a = Agents::default();
        let t = Instant::now();
        let notice = a.apply(&ev("PreToolUse", Some("AskUserQuestion"), None), t);
        assert_eq!(notice, Some(Notice::NeedsYou { project: "notch".into(), what: "Needs your answer" }));
        assert_eq!(a.active().map(|s| s.state), Some(AgentState::Waiting));
        // After being answered, the tool finishes and the agent goes back to work.
        a.apply(&ev("PostToolUse", Some("AskUserQuestion"), None), t);
        assert_eq!(a.active().map(|s| (s.state, s.activity.as_str())), Some((AgentState::Working, THINKING)));
    }

    #[test]
    fn history_and_prompt() {
        let mut a = Agents::default();
        let t = Instant::now();
        let mut prompt = ev("UserPromptSubmit", None, None);
        prompt.message = Some("Tambahkan widget musik".into());
        a.apply(&prompt, t);
        for f in ["a.rs", "b.rs", "c.rs", "d.rs"] {
            a.apply(&ev("PostToolUse", Some("Edit"), Some(f)), t);
        }
        let s = a.active().unwrap();
        assert_eq!(s.prompt.as_deref(), Some("Tambahkan widget musik"));
        assert_eq!(s.history, ["Editing d.rs", "Editing c.rs", "Editing b.rs"]);
    }

    #[test]
    fn active_sessions_come_first() {
        let mut a = Agents::default();
        let t = Instant::now();
        let mut done = ev("Stop", None, None);
        done.session = "old".into();
        a.apply(&ev("PreToolUse", Some("Bash"), Some("ls")), t);
        a.apply(&done, t + Duration::from_secs(5));
        assert_eq!(a.sessions()[0].id, "old");
        assert_eq!(a.sessions_by_state()[0].id, "s1");
    }

    #[test]
    fn model_names() {
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        // An empty or truncated ID must not make the app exit.
        assert_eq!(model_name(""), "");
        assert_eq!(model_name("claude-"), "claude-");
        assert_eq!(model_name("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(model_name("claude-3-5-haiku-20241022"), "Haiku 3.5");
        assert_eq!(model_name("claude-fable-5-1[1m]"), "Fable 5.1");
        assert_eq!(model_name("gpt-x"), "Gpt");
    }

    #[test]
    fn descriptions() {
        assert_eq!(describe("Bash", Some("npm test")), "Running npm test");
        assert_eq!(describe("Bash", Some("cargo test --workspace")), "Running cargo test");
        assert_eq!(describe("Bash", Some("git -C x status")), "Running git");
        assert_eq!(describe("PowerShell", Some("$s=\"C:\\Users\"; & $s")), "Running a command");
        assert_eq!(describe("TodoWrite", Some("TodoWrite")), "TodoWrite");
        assert_eq!(describe("mcp__Claude_Browser__javascript_tool", Some("Claude_Browser · javascript_tool")), "Using Claude Browser");
        assert_eq!(describe("TodoWrite", None), "TodoWrite");
    }
}

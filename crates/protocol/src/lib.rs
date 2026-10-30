//! Messages between `notch-bridge` (called by agent hooks and statusline) and the notch app.
//!
//! Transport: one named pipe connection per event, one JSON line in each direction.
//! For `PermissionRequest`, the notch replies with one [`Reply`] line containing the user's decision.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Pipe name for the user with SID `user_sid`. Per user, so that on a PC with several accounts
/// each notch has its own pipe (the pipe can also only be opened by its owner).
pub fn pipe_name(user_sid: &str) -> String {
    format!(r"\\.\pipe\notch-agent-{user_sid}")
}
/// The Claude (desktop) app's `claude_desktop_config.json`, where the MCP server for Chat is registered.
/// The Store version keeps its data in `%LOCALAPPDATA%\Claude-Data\claude_<package>` (the old folder
/// `%APPDATA%\Claude` then holds only a marker); older versions use `%APPDATA%\Claude` or the package's
/// virtual folder. `None` if the Claude app is not installed.
pub fn claude_desktop_config() -> Option<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    const FILE: &str = "claude_desktop_config.json";
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let subdirs = |dir: &Path, prefix: &str| -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut found: Vec<PathBuf> = entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(prefix)).map(|e| e.path()).filter(|p| p.is_dir()).collect();
        found.sort();
        found
    };
    let mut candidates = Vec::new();
    if let Some(local) = env("LOCALAPPDATA") {
        candidates.extend(subdirs(&local.join("Claude-Data"), "claude_"));
        candidates.extend(subdirs(&local.join("Packages"), "Claude_").into_iter().map(|p| p.join("LocalCache").join("Roaming").join("Claude")));
    }
    if let Some(roaming) = env("APPDATA") {
        candidates.push(roaming.join("Claude"));
    }
    // An existing file takes priority; if none exists, the first data folder that does.
    candidates.iter().map(|d| d.join(FILE)).find(|f| f.is_file()).or_else(|| candidates.iter().find(|d| d.is_dir()).map(|d| d.join(FILE)))
}

/// Apps other than Claude Code that can use the notch MCP server (`notch-bridge mcp`). Claude Code
/// is connected via hooks; these apps are simply registered in their own MCP config files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum McpApp {
    /// Chat in the Claude app (desktop).
    ClaudeApp,
    Codex,
    /// Google's Antigravity agent IDE.
    Antigravity,
    /// Copilot agent mode in VS Code.
    VsCode,
}

impl McpApp {
    pub const ALL: [McpApp; 4] = [McpApp::ClaudeApp, McpApp::Codex, McpApp::Antigravity, McpApp::VsCode];

    /// Name for the `notch-bridge install-mcp <key>` argument.
    pub fn key(self) -> &'static str {
        match self {
            McpApp::ClaudeApp => "claude-app",
            McpApp::Codex => "codex",
            McpApp::Antigravity => "antigravity",
            McpApp::VsCode => "vscode",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            McpApp::ClaudeApp => "Claude app",
            McpApp::Codex => "Codex",
            McpApp::Antigravity => "Antigravity",
            McpApp::VsCode => "VS Code",
        }
    }

    /// The server list object in the JSON config: VS Code uses `servers`, the others `mcpServers`.
    pub fn servers_key(self) -> &'static str {
        if self == McpApp::VsCode {
            "servers"
        } else {
            "mcpServers"
        }
    }

    /// Codex's config is TOML; the others are JSON with an `mcpServers` object (VS Code: `servers`).
    pub fn is_toml(self) -> bool {
        self == McpApp::Codex
    }

    /// This app's MCP config file (may not exist yet). `None` if the app isn't installed,
    /// detected from its data folder.
    pub fn config(self) -> Option<std::path::PathBuf> {
        use std::path::PathBuf;
        let home = || std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
        let (dir, file) = match self {
            McpApp::ClaudeApp => return claude_desktop_config(),
            McpApp::Codex => (std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| home().map(|h| h.join(".codex")))?, "config.toml"),
            McpApp::Antigravity => (home()?.join(".gemini").join("antigravity"), "mcp_config.json"),
            // User-level MCP config ("MCP: Open User Configuration").
            McpApp::VsCode => (PathBuf::from(std::env::var_os("APPDATA")?).join("Code").join("User"), "mcp.json"),
        };
        dir.is_dir().then(|| dir.join(file))
    }
}

/// How long notch waits for the user to answer a permission prompt before handing back to the agent's built-in dialog.
/// In a terminal, the built-in dialog appears only after the hook finishes, so don't wait too long.
pub const PERMISSION_WAIT_SECS: u64 = 30;
/// Wait limit for an answer to a question or plan approval; longer because it needs to be read first.
pub const QUESTION_WAIT_SECS: u64 = 60;
/// Wait limit for answers to `ask_user` and `request_approval` from MCP: the agent really stops
/// to wait, so this can be longer than for hook cards.
pub const MCP_WAIT_SECS: u64 = 100;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    Hook(HookEvent),
    Status(Status),
    /// An MCP tool call from the agent (`notch-bridge mcp`); always answered with one [`Reply`].
    Mcp(Mcp),
}

/// An MCP tool call. `agent`: the MCP client name, e.g. "Claude".
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Mcp {
    pub agent: String,
    pub call: McpCall,
}

/// Notch MCP tool. Tool name = the variant in snake_case.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum McpCall {
    Notify {
        title: String,
        #[serde(default)]
        detail: Option<String>,
    },
    AskUser {
        question: String,
        options: Vec<String>,
        #[serde(default)]
        multi: bool,
    },
    RequestApproval {
        action: String,
        #[serde(default)]
        detail: Option<String>,
    },
    Progress {
        label: String,
        #[serde(default)]
        percent: Option<f32>,
    },
    AddTask {
        text: String,
    },
    CompleteTask {
        task: String,
    },
    GetStatus,
}

impl McpCall {
    /// How long the notch waits for the user before replying without an answer.
    pub fn wait_secs(&self) -> u64 {
        match self {
            McpCall::AskUser { .. } | McpCall::RequestApproval { .. } => MCP_WAIT_SECS,
            _ => 5,
        }
    }
}

/// Summary of one hook event. The bridge has already summarized the raw input (which can contain full file
/// contents) so that what goes through the pipe stays small.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct HookEvent {
    /// Originating agent, e.g. "claude".
    pub source: String,
    /// Hook event name as-is, e.g. "PreToolUse".
    pub event: String,
    pub session: String,
    /// Working directory name (not the full path).
    pub project: String,
    /// Project folder path (repo root for worktrees), for opening it from the notch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Action summary for display, e.g. "npm test" or "auth.service.ts".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The command's explanation from the agent itself (`description` on the Bash/PowerShell tool), e.g.
    /// "Build the debug binaries". Clearer than the command itself for live activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The full Bash/PowerShell command for the permission card; `summary` is only the first line,
    /// truncated. Only filled for `PermissionRequest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_type: Option<String>,
    /// Current context tokens (input + cache) from the last reply in the session transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// Model ID from the transcript, e.g. "claude-opus-5-5".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `AskUserQuestion` questions that can be answered from the notch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<Question>>,
    /// The `ExitPlanMode` plan awaiting approval, one line per step (no Markdown markers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

/// A single multiple-choice question from the agent.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Question {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<Choice>,
    /// Multiple choices allowed.
    #[serde(default)]
    pub multi: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Choice {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl HookEvent {
    pub fn is_permission(&self) -> bool {
        self.event == "PermissionRequest"
    }

    /// The agent asks a question or requests plan approval and it can be answered from the notch.
    pub fn asks_user(&self) -> bool {
        self.event == "PreToolUse" && (self.questions.as_ref().is_some_and(|q| !q.is_empty()) || self.tool.as_deref() == Some("ExitPlanMode"))
    }

    /// The bridge waits for the notch's reply to this event.
    pub fn wants_reply(&self) -> bool {
        self.is_permission() || self.asks_user()
    }

    /// Reply wait limit, seconds.
    pub fn wait_secs(&self) -> u64 {
        if self.asks_user() {
            QUESTION_WAIT_SECS
        } else {
            PERMISSION_WAIT_SECS
        }
    }
}

/// Statusline data: model, context, and quota usage.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Status {
    pub source: String,
    pub session: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour_resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day_resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
}

/// Notch reply. `decision: None` means the user did not answer: the agent uses its built-in dialog.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Reply {
    #[serde(default)]
    pub decision: Option<Decision>,
    /// Answer to a question: question text → chosen label (multi-select joined with commas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<BTreeMap<String, String>>,
    /// MCP tool result that is not a decision, e.g. the status or an added task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip() {
        let m = Message::Hook(HookEvent {
            source: "claude".into(),
            event: "PreToolUse".into(),
            session: "s1".into(),
            project: "notch".into(),
            tool: Some("Bash".into()),
            summary: Some("npm test".into()),
            ..Default::default()
        });
        let line = serde_json::to_string(&m).unwrap();
        assert!(line.starts_with(r#"{"kind":"hook","#));
        assert_eq!(serde_json::from_str::<Message>(&line).unwrap(), m);
    }

    #[test]
    fn reply_without_decision() {
        assert_eq!(serde_json::from_str::<Reply>("{}").unwrap(), Reply::default());
        assert_eq!(serde_json::to_string(&Reply { decision: Some(Decision::Allow), ..Reply::default() }).unwrap(), r#"{"decision":"allow"}"#);
    }

    #[test]
    fn mcp_calls_by_tool_name() {
        let call: McpCall = serde_json::from_value(serde_json::json!({ "tool": "ask_user", "question": "Ship?", "options": ["Yes", "No"] })).unwrap();
        assert_eq!(call, McpCall::AskUser { question: "Ship?".into(), options: vec!["Yes".into(), "No".into()], multi: false });
        assert_eq!(call.wait_secs(), MCP_WAIT_SECS);
        let status: McpCall = serde_json::from_value(serde_json::json!({ "tool": "get_status" })).unwrap();
        assert_eq!((status.clone(), status.wait_secs()), (McpCall::GetStatus, 5));
        let m = Message::Mcp(Mcp { agent: "Claude".into(), call: status });
        assert_eq!(serde_json::from_str::<Message>(&serde_json::to_string(&m).unwrap()).unwrap(), m);
    }

    #[test]
    fn only_questions_and_plans_wait_longer() {
        let mut ev = HookEvent { event: "PreToolUse".into(), tool: Some("Bash".into()), ..Default::default() };
        assert!(!ev.wants_reply());
        ev.tool = Some("ExitPlanMode".into());
        assert!(ev.asks_user() && ev.wait_secs() == QUESTION_WAIT_SECS);
        ev.tool = Some("AskUserQuestion".into());
        assert!(!ev.asks_user(), "tanpa pertanyaan yang terbaca, biarkan UI agent");
        ev.questions = Some(vec![Question { question: "Which?".into(), ..Default::default() }]);
        assert!(ev.wants_reply());
        let perm = HookEvent { event: "PermissionRequest".into(), ..Default::default() };
        assert_eq!(perm.wait_secs(), PERMISSION_WAIT_SECS);
    }
}

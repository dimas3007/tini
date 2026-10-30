//! `notch-bridge mcp`: an MCP server over stdio, so agents (Claude Code, Codex, and others) can
//! use the notch: send notifications, ask questions, request approval, report progress, change tasks, and read
//! user context. Each tool call is forwarded to the notch over the same named pipe as the
//! hooks, and the answer is returned to the agent.
//!
//! No process keeps running other than the one the agent itself launches. If the notch isn't
//! running, tools return a plain error message and the agent can carry on.

use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};

use notch_protocol::{Decision, Mcp, McpCall, Message, Reply};
use serde_json::{json, Value};

/// MCP protocol version used when the client doesn't specify one.
const PROTOCOL: &str = "2025-06-18";

const INSTRUCTIONS: &str = "Notch is a small panel at the top of the user's Windows screen. Use it to reach the user when they may not be watching the chat: \
notify for milestones or a finished long task, ask_user for a quick multiple-choice decision, request_approval before something risky or irreversible, \
progress for long multi-step work. get_status tells you the local time, today's tasks, whether a focus timer is running and whether the screen is locked; \
check it before interrupting. Keep messages short: the notch shows one or two lines.";

fn tools() -> Value {
    json!([
        {
            "name": "notify",
            "description": "Show a short message in the user's notch for a few seconds. Use for milestones or when a long task finishes, not for every step.",
            "inputSchema": { "type": "object", "properties": {
                "title": { "type": "string", "description": "One short line, e.g. 'Tests pass'." },
                "detail": { "type": "string", "description": "Optional second line, e.g. the project or a number." }
            }, "required": ["title"] }
        },
        {
            "name": "ask_user",
            "description": "Ask the user a multiple-choice question in the notch and wait for the answer (up to about 100 seconds). Returns the chosen option (several, comma-separated, when multi is true), or says the user did not answer there.",
            "inputSchema": { "type": "object", "properties": {
                "question": { "type": "string" },
                "options": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 4, "description": "2 to 4 short labels." },
                "multi": { "type": "boolean", "description": "Allow picking more than one option." }
            }, "required": ["question", "options"] }
        },
        {
            "name": "request_approval",
            "description": "Ask the user to approve an action before you do it, in the notch. Waits up to about 100 seconds. Returns approved, declined, or no answer; only proceed when approved.",
            "inputSchema": { "type": "object", "properties": {
                "action": { "type": "string", "description": "What you want to do, e.g. 'git push origin main'." },
                "detail": { "type": "string", "description": "Optional reason or scope." }
            }, "required": ["action"] }
        },
        {
            "name": "progress",
            "description": "Show what you are working on in the notch's live view, optionally with a percentage. Update it at meaningful steps of long work.",
            "inputSchema": { "type": "object", "properties": {
                "label": { "type": "string", "description": "e.g. 'Migrating the database'." },
                "percent": { "type": "number", "minimum": 0, "maximum": 100 }
            }, "required": ["label"] }
        },
        {
            "name": "add_task",
            "description": "Add tasks to the user's Today list. Natural language works, e.g. 'review PR tomorrow at 10, then call Rina'.",
            "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] }
        },
        {
            "name": "complete_task",
            "description": "Mark one of today's open tasks as done, matched by (part of) its title.",
            "inputSchema": { "type": "object", "properties": { "task": { "type": "string" } }, "required": ["task"] }
        },
        {
            "name": "get_status",
            "description": "Read the user's current context from the notch: local time, today's tasks, focus timer, and whether the screen is locked.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

/// Human-readable agent name from `clientInfo.name`, e.g. "claude-code" → "Claude Code". Other Claude
/// clients ("claude-ai") are Chat in the Claude app.
fn agent_name(client: &str) -> String {
    let lower = client.to_lowercase();
    let known = [
        ("claude-code", "Claude Code"),
        ("claude", "Claude app"),
        ("codex", "Codex"),
        // Before "gemini": the Antigravity client may mention Gemini too.
        ("antigravity", "Antigravity"),
        // Before "vscode": the Cursor client is named "cursor-vscode".
        ("cursor", "Cursor"),
        ("visual studio code", "VS Code"),
        ("vscode", "VS Code"),
        ("gemini", "Gemini"),
        ("hermes", "Hermes"),
        ("grok", "Grok"),
    ];
    for (key, name) in known {
        if lower.contains(key) {
            return name.to_string();
        }
    }
    if client.is_empty() {
        "Agent".into()
    } else {
        client.to_string()
    }
}

/// Tool result text for the agent, from the notch's reply.
fn result_text(call: &McpCall, reply: Reply) -> String {
    if let Some(text) = reply.text {
        return text;
    }
    match (call, reply.decision) {
        (McpCall::AskUser { question, .. }, Some(Decision::Allow)) => {
            let answer = reply.answers.as_ref().and_then(|a| a.get(question)).cloned().unwrap_or_default();
            format!("The user chose: {answer}")
        }
        (McpCall::AskUser { .. }, _) => "The user did not answer in the notch (no reply in time, or they chose to answer in the chat). Ask in the conversation instead.".into(),
        (McpCall::RequestApproval { .. }, Some(Decision::Allow)) => "Approved by the user. You may proceed.".into(),
        (McpCall::RequestApproval { action, .. }, Some(Decision::Deny)) => format!("Declined by the user. Do not do this: {action}"),
        (McpCall::RequestApproval { .. }, None) => "No answer from the user in the notch. Do not proceed; ask in the conversation instead.".into(),
        _ => "Done.".into(),
    }
}

/// A single tool call: (result text, whether it is an error).
fn call_tool(agent: &str, name: &str, args: &Value) -> (String, bool) {
    let mut obj = args.as_object().cloned().unwrap_or_default();
    obj.insert("tool".into(), Value::String(name.into()));
    let call: McpCall = match serde_json::from_value(Value::Object(obj)) {
        Ok(c) => c,
        Err(e) => return (format!("Invalid arguments for {name}: {e}"), true),
    };
    if let McpCall::AskUser { options, .. } = &call {
        if !(2..=4).contains(&options.len()) {
            return ("ask_user needs 2 to 4 options.".into(), true);
        }
    }
    let Some(mut pipe) = crate::connect() else {
        return ("The notch app is not running, so the user did not see this. Continue in the conversation.".into(), true);
    };
    if !crate::send(&mut pipe, &Message::Mcp(Mcp { agent: agent.to_string(), call: call.clone() })) {
        return ("Could not reach the notch.".into(), true);
    }
    let mut line = String::new();
    let reply = match BufReader::new(pipe).read_line(&mut line) {
        Ok(n) if n > 0 => serde_json::from_str::<Reply>(&line).unwrap_or_default(),
        _ => Reply::default(),
    };
    (result_text(&call, reply), false)
}

fn write(out: &Arc<Mutex<std::io::Stdout>>, msg: &Value) {
    if let Ok(mut o) = out.lock() {
        let _ = writeln!(o, "{msg}");
        let _ = o.flush();
    }
}

/// Main loop: one JSON-RPC message per line. Tool calls run on their own thread, because
/// `ask_user` and `request_approval` can wait a long time.
pub fn serve() -> i32 {
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let mut agent = "Agent".to_string();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg["method"].as_str().unwrap_or("").to_string();
        let Some(id) = msg.get("id").cloned() else { continue }; // notifications are not replied to
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "initialize" => {
                agent = agent_name(params["clientInfo"]["name"].as_str().unwrap_or(""));
                let version = params["protocolVersion"].as_str().unwrap_or(PROTOCOL);
                write(
                    &out,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": {
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "notch", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS
                    } }),
                );
            }
            "ping" => write(&out, &json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
            "tools/list" => write(&out, &json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools() } })),
            "tools/call" => {
                let (out, agent) = (out.clone(), agent.clone());
                std::thread::spawn(move || {
                    let name = params["name"].as_str().unwrap_or("");
                    let (text, is_error) = call_tool(&agent, name, &params["arguments"]);
                    write(&out, &json!({ "jsonrpc": "2.0", "id": id, "result": {
                        "content": [{ "type": "text", "text": text }],
                        "isError": is_error
                    } }));
                });
            }
            _ => write(&out, &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Method not found: {method}") } })),
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn agent_names() {
        assert_eq!(agent_name("claude-code"), "Claude Code");
        assert_eq!(agent_name("claude-ai"), "Claude app");
        assert_eq!(agent_name("codex-mcp-client"), "Codex");
        assert_eq!(agent_name("antigravity"), "Antigravity");
        assert_eq!(agent_name("Visual Studio Code"), "VS Code");
        assert_eq!(agent_name("cursor-vscode"), "Cursor");
        assert_eq!(agent_name("gemini-cli-mcp-client"), "Gemini");
        assert_eq!(agent_name(""), "Agent");
        assert_eq!(agent_name("my-bot"), "my-bot");
    }

    #[test]
    fn tool_list_matches_calls() {
        for tool in tools().as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            // The minimal example arguments for each tool must parse as a McpCall.
            let args = match name {
                "notify" => json!({ "title": "x" }),
                "ask_user" => json!({ "question": "x", "options": ["a", "b"] }),
                "request_approval" => json!({ "action": "x" }),
                "progress" => json!({ "label": "x", "percent": 40 }),
                "add_task" => json!({ "text": "x" }),
                "complete_task" => json!({ "task": "x" }),
                _ => json!({}),
            };
            let mut obj = args.as_object().unwrap().clone();
            obj.insert("tool".into(), json!(name));
            assert!(serde_json::from_value::<McpCall>(Value::Object(obj)).is_ok(), "{name}");
        }
    }

    #[test]
    fn replies_become_plain_answers() {
        let ask = McpCall::AskUser { question: "Ship?".into(), options: vec!["Yes".into(), "No".into()], multi: false };
        let answers: BTreeMap<String, String> = [("Ship?".to_string(), "Yes".to_string())].into_iter().collect();
        assert_eq!(result_text(&ask, Reply { decision: Some(Decision::Allow), answers: Some(answers), text: None }), "The user chose: Yes");
        assert!(result_text(&ask, Reply::default()).starts_with("The user did not answer"));
        let approve = McpCall::RequestApproval { action: "git push".into(), detail: None };
        assert!(result_text(&approve, Reply { decision: Some(Decision::Deny), ..Reply::default() }).contains("git push"));
        assert_eq!(result_text(&McpCall::GetStatus, Reply { text: Some("ok".into()), ..Reply::default() }), "ok");
    }
}

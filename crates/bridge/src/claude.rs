//! Translates Claude Code hook and statusline input into notch messages.

use notch_protocol::{Choice, Decision, HookEvent, Question, Reply, Status};
use serde_json::{json, Value};

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn basename(path: &str) -> String {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Project name from the working directory: the project's folder name (see `project_dir`).
fn project_name(cwd: &str) -> String {
    basename(&project_dir(cwd))
}

/// Project folder from the working directory: a Claude Code worktree (`<repo>/.claude/worktrees/<name>`)
/// is replaced by the repo root, and repo subfolders (a session that moved into `src/`) go up to the repo root. So
/// the project name stays the same, and projects opened from the notch survive after the worktree is cleaned up.
fn project_dir(cwd: &str) -> String {
    let path = cwd.replace('\\', "/");
    let root = match path.find("/.claude/worktrees/") {
        Some(i) => &cwd[..i],
        None => cwd,
    };
    let root = root.trim_end_matches(['/', '\\']);
    let repo = std::path::Path::new(root).ancestors().find(|p| p.join(".git").exists());
    repo.map_or_else(|| root.to_string(), |p| p.display().to_string())
}

/// Truncates to `max` characters (not bytes) with an ellipsis.
fn clip(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        line.to_string()
    } else {
        format!("{}…", line.chars().take(max - 1).collect::<String>())
    }
}

/// Summary of a tool action that fits on one short line in the notch.
pub fn summarize(tool: &str, input: &Value) -> String {
    let field = |k: &str| input.get(k).and_then(Value::as_str);
    let s = match tool {
        "Bash" | "PowerShell" => field("command").map(|c| clip(c, 60)),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            field("file_path").or(field("notebook_path")).map(basename)
        }
        "Glob" | "Grep" => field("pattern").map(|p| clip(p, 50)),
        "WebFetch" => field("url").map(|u| u.split('/').nth(2).unwrap_or(u).to_string()),
        "WebSearch" => field("query").map(|q| clip(q, 50)),
        "Task" | "Agent" => field("description").map(|d| clip(d, 50)),
        _ => None,
    };
    if let Some(s) = s {
        return s;
    }
    // MCP tool: mcp__<server>__<tool> → "server · tool"
    if let Some(rest) = tool.strip_prefix("mcp__") {
        if let Some((server, name)) = rest.split_once("__") {
            return format!("{server} · {name}");
        }
    }
    tool.to_string()
}

/// The `AskUserQuestion` questions from `tool_input`. Empty if the format is not recognized.
fn questions(tool_input: &Value) -> Vec<Question> {
    let list = tool_input.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
    list.iter()
        .filter_map(|q| {
            let options = q.get("options").and_then(Value::as_array)?;
            Some(Question {
                question: text(q, "question")?,
                header: text(q, "header"),
                options: options.iter().filter_map(|o| Some(Choice { label: text(o, "label")?, description: text(o, "description") })).collect(),
                multi: q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .filter(|q| !q.options.is_empty())
        .collect()
}

/// Multi-line text for a card, truncated to `max` characters (not bytes) with an ellipsis. The card
/// scrolls long text; this limit only keeps the pipe message small.
/// Commands this long or longer are not shown as a card: too long to
/// review in the notch, so Claude Code uses its own dialog.
pub const MAX_CARD_COMMAND: usize = 20_000;

/// A permission request whose content is too long to show in full on the card.
pub fn too_long_for_card(ev: &HookEvent) -> bool {
    ev.is_permission() && ev.command.as_ref().is_some_and(|c| c.chars().count() > MAX_CARD_COMMAND)
}

fn clip_block(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

/// Plan for the notch card: each line that has text, without leading Markdown markers.
fn plan_text(tool_input: &Value) -> Option<String> {
    let plan = tool_input.get("plan").and_then(Value::as_str)?;
    let lines: Vec<&str> = plan.lines().map(|l| l.trim().trim_start_matches(['#', '-', '*', '>', ' '])).filter(|l| !l.is_empty()).collect();
    Some(clip_block(&lines.join("\n"), 2000)).filter(|p| !p.is_empty())
}

/// `PreToolUse` hook output for answers from the notch: questions are answered via `updatedInput`
/// (the original questions + `answers`), a plan is approved with its input as-is or rejected so Claude
/// keeps planning. `None`: no decision, the agent uses its own UI.
pub fn ask_output(tool: &str, tool_input: &Value, reply: &Reply) -> Option<Value> {
    let allow = |updated: Value| json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": updated } });
    match (tool, reply.decision?) {
        ("AskUserQuestion", Decision::Allow) => {
            let mut updated = tool_input.clone();
            updated["answers"] = json!(reply.answers.clone()?);
            Some(allow(updated))
        }
        ("ExitPlanMode", Decision::Allow) => Some(allow(tool_input.clone())),
        ("ExitPlanMode", Decision::Deny) => Some(json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "The user wants to keep planning. Ask what they would like to change."
        } })),
        _ => None,
    }
}

/// Contents of the first `<name>…</name>` tag.
fn tag_body<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{name}>"))?;
    Some(&text[start..start + end])
}

/// The first line the user actually wrote. Clients (e.g. the Claude desktop app) inject
/// tagged blocks into the prompt, such as `<system-reminder>…</system-reminder>` or other session events;
/// those blocks are dropped. Only tags with "-" or "_" in the name count as injected, so plain HTML
/// typed by the user (`<div>`) stays intact. A slash command becomes "/name args".
/// `None` if there is no user text at all (a system-triggered turn).
fn human_prompt(prompt: &str) -> Option<String> {
    if let Some(name) = tag_body(prompt, "command-name") {
        let args = tag_body(prompt, "command-args").unwrap_or("");
        return Some(format!("{} {}", name.trim(), args.trim()).trim().to_string());
    }
    let mut out = String::new();
    let mut rest = prompt;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect();
        let injected = name.starts_with(|c: char| c.is_ascii_lowercase()) && name.contains(['-', '_']);
        if !injected {
            out.push('<');
            rest = after;
            continue;
        }
        // A complete block is dropped; for a tag with no closing tag, only the tag itself.
        let close = format!("</{name}>");
        rest = match after.find(&close) {
            Some(end) => &after[end + close.len()..],
            None => after.find('>').map_or("", |e| &after[e + 1..]),
        };
    }
    out.push_str(rest);
    out.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

pub fn to_event(input: &Value) -> HookEvent {
    let tool = text(input, "tool_name");
    let tool_input = input.get("tool_input").unwrap_or(&Value::Null);
    let summary = tool.as_deref().map(|t| summarize(t, tool_input));
    let description = match tool.as_deref() {
        Some("Bash" | "PowerShell") => tool_input.get("description").and_then(Value::as_str).map(|d| clip(d, 60)).filter(|d| !d.is_empty()),
        _ => None,
    };
    let message = text(input, "message").or_else(|| text(input, "prompt").as_deref().and_then(human_prompt).map(|p| clip(&p, 80)));
    HookEvent {
        source: "claude".into(),
        event: text(input, "hook_event_name").unwrap_or_default(),
        session: text(input, "session_id").unwrap_or_default(),
        project: text(input, "cwd").map(|c| project_name(&c)).unwrap_or_default(),
        cwd: text(input, "cwd").filter(|c| !c.is_empty()).map(|c| project_dir(&c)),
        notification_type: text(input, "notification_type"),
        questions: match tool.as_deref() {
            Some("AskUserQuestion") => Some(questions(input.get("tool_input").unwrap_or(&Value::Null))).filter(|q| !q.is_empty()),
            _ => None,
        },
        plan: match tool.as_deref() {
            Some("ExitPlanMode") => plan_text(input.get("tool_input").unwrap_or(&Value::Null)),
            _ => None,
        },
        // The permission card shows exactly what will be approved: full command, full path
        // (not just the file name), full URL.
        command: match (tool.as_deref(), input.get("hook_event_name").and_then(Value::as_str)) {
            (Some("Bash" | "PowerShell"), Some("PermissionRequest")) => tool_input.get("command").and_then(Value::as_str).map(|c| c.trim().to_string()),
            (Some("Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit"), Some("PermissionRequest")) => {
                tool_input.get("file_path").or_else(|| tool_input.get("notebook_path")).and_then(Value::as_str).map(str::to_string)
            }
            (Some("WebFetch"), Some("PermissionRequest")) => tool_input.get("url").and_then(Value::as_str).map(str::to_string),
            _ => None,
        },
        tool,
        summary,
        description,
        message,
        ..Default::default()
    }
}

/// Context tokens and model from the last assistant reply in the transcript (JSONL). Context = input
/// + cache tokens sent to the model in that turn. Subagent (sidechain) replies are skipped
/// because their context is separate from the main session.
pub fn transcript_context(text: &str) -> Option<(u64, Option<String>)> {
    text.lines().rev().find_map(|line| {
        if !line.contains("\"usage\"") {
            return None;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        if v.get("type").and_then(Value::as_str) != Some("assistant") || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let m = v.get("message")?;
        let u = m.get("usage")?;
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let tokens = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
        (tokens > 0).then(|| (tokens, m.get("model").and_then(Value::as_str).map(str::to_string)))
    })
}

/// `PermissionRequest` hook output for a decision from the notch.
pub fn permission_output(decision: Decision) -> Value {
    let decision = match decision {
        Decision::Allow => json!({ "behavior": "allow" }),
        Decision::Deny => json!({ "behavior": "deny", "message": "Denied from the notch." }),
    };
    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
}

pub fn to_status(input: &Value) -> Status {
    let num = |path: &[&str]| path.iter().try_fold(input, |v, k| v.get(*k)).and_then(Value::as_f64);
    let dir = input.pointer("/workspace/current_dir").and_then(Value::as_str).or_else(|| input.get("cwd").and_then(Value::as_str));
    Status {
        source: "claude".into(),
        session: text(input, "session_id").unwrap_or_default(),
        project: dir.map(project_name).unwrap_or_default(),
        model: input.pointer("/model/display_name").and_then(Value::as_str).map(str::to_string),
        context_pct: num(&["context_window", "used_percentage"]).map(|v| v as f32),
        five_hour_pct: num(&["rate_limits", "five_hour", "used_percentage"]).map(|v| v as f32),
        five_hour_resets_at: num(&["rate_limits", "five_hour", "resets_at"]).map(|v| v as u64),
        seven_day_pct: num(&["rate_limits", "seven_day", "used_percentage"]).map(|v| v as f32),
        seven_day_resets_at: num(&["rate_limits", "seven_day", "resets_at"]).map(|v| v as u64),
        cost_usd: num(&["cost", "total_cost_usd"]),
    }
}

/// Status line text for the Claude Code terminal, e.g. "Opus · ctx 8% · 5h 23% · 7d 41%".
pub fn statusline_text(s: &Status) -> String {
    let mut parts = vec![s.model.clone().unwrap_or_else(|| "Claude".into())];
    if let Some(c) = s.context_pct {
        parts.push(format!("ctx {c:.0}%"));
    }
    if let Some(p) = s.five_hour_pct {
        parts.push(format!("5h {p:.0}%"));
    }
    if let Some(p) = s.seven_day_pct {
        parts.push(format!("7d {p:.0}%"));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn cards_show_full_targets_and_skip_huge_commands() {
        let edit = to_event(&json!({ "hook_event_name": "PermissionRequest", "tool_name": "Edit", "tool_input": { "file_path": "C:\\Windows\\System32\\drivers\\etc\\hosts" } }));
        assert_eq!(edit.command.as_deref(), Some("C:\\Windows\\System32\\drivers\\etc\\hosts"));
        let long = "x".repeat(MAX_CARD_COMMAND + 1);
        let bash = to_event(&json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": { "command": long } }));
        assert!(too_long_for_card(&bash));
        let short = to_event(&json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": { "command": "git push" } }));
        assert!(!too_long_for_card(&short));
    }

    use super::*;

    #[test]
    fn prompts_without_injected_blocks() {
        let p = "<system-reminder>\nThe session finished.\n</system-reminder>\n<side-sessions-event>None working</side-sessions-event>";
        assert_eq!(human_prompt(p), None);
        assert_eq!(human_prompt("<artifact-view-context a=\"1\">{}</artifact-view-context>\nfix the <div> padding"), Some("fix the <div> padding".into()));
        assert_eq!(human_prompt("<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>main</command-args>"), Some("/review main".into()));
        assert_eq!(human_prompt("tolong <b>rapikan</b> README"), Some("tolong <b>rapikan</b> README".into()));
        let e = to_event(&json!({ "hook_event_name": "UserPromptSubmit", "prompt": "<system-reminder>x</system-reminder>" }));
        assert_eq!(e.message, None);
    }

    #[test]
    fn commands_carry_their_description() {
        let e = to_event(&json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": { "command": "cd x && cargo build", "description": "Build the debug binaries" } }));
        assert_eq!(e.summary.as_deref(), Some("cd x && cargo build"));
        assert_eq!(e.description.as_deref(), Some("Build the debug binaries"));
        assert_eq!(e.command, None, "the full command is only for permission cards");
        let long = format!("cargo build \\\n  --release {}", "x".repeat(100));
        let perm = to_event(&json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": { "command": long } }));
        assert_eq!(perm.summary.as_deref(), Some("cargo build \\"));
        assert_eq!(perm.command.as_deref(), Some(long.as_str()));
    }

    #[test]
    fn summaries() {
        assert_eq!(summarize("Bash", &json!({"command": "npm test\necho done"})), "npm test");
        assert_eq!(summarize("Edit", &json!({"file_path": "C:\\proj\\src\\auth.service.ts"})), "auth.service.ts");
        assert_eq!(summarize("WebFetch", &json!({"url": "https://code.claude.com/docs/en/hooks"})), "code.claude.com");
        assert_eq!(summarize("mcp__github__create_issue", &json!({})), "github · create_issue");
        assert_eq!(summarize("TodoWrite", &json!({})), "TodoWrite");
        let long = "x".repeat(100);
        assert_eq!(summarize("Bash", &json!({ "command": long })).chars().count(), 60);
    }

    #[test]
    fn hook_input_to_event() {
        let input = json!({
            "session_id": "abc",
            "cwd": "C:\\Users\\me\\My Projects\\notch",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": "cargo test" }
        });
        let ev = to_event(&input);
        assert_eq!((ev.event.as_str(), ev.project.as_str(), ev.summary.as_deref()), ("PreToolUse", "notch", Some("cargo test")));
        assert_eq!(ev.cwd.as_deref(), Some("C:\\Users\\me\\My Projects\\notch"));
        let worktree = to_event(&json!({ "cwd": "C:\\code\\notch\\.claude\\worktrees\\agitated-grothendieck-1b387c" }));
        assert_eq!(worktree.project, "notch");
        assert_eq!(worktree.cwd.as_deref(), Some("C:\\code\\notch"));
        assert_eq!(project_name("/home/me/app/.claude/worktrees/x"), "app");
        let prompt = to_event(&json!({ "hook_event_name": "UserPromptSubmit", "prompt": "tolong rapikan README" }));
        assert_eq!(prompt.message.as_deref(), Some("tolong rapikan README"));
    }

    #[test]
    fn status_fields() {
        let input = json!({
            "session_id": "abc",
            "model": { "display_name": "Opus" },
            "workspace": { "current_dir": "/home/me/notch" },
            "context_window": { "used_percentage": 8 },
            "rate_limits": { "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600 } },
            "cost": { "total_cost_usd": 0.01 }
        });
        let s = to_status(&input);
        assert_eq!((s.project.as_str(), s.five_hour_pct, s.seven_day_pct), ("notch", Some(23.5), None));
        assert_eq!(statusline_text(&s), "Opus · ctx 8% · 5h 24%");
    }

    #[test]
    fn context_from_transcript() {
        let main = |tokens: u64| json!({"type": "assistant", "message": {"model": "claude-opus-5-5", "usage": {"input_tokens": 2, "cache_creation_input_tokens": 1000, "cache_read_input_tokens": tokens}}}).to_string();
        let side = json!({"type": "assistant", "isSidechain": true, "message": {"usage": {"input_tokens": 5}}}).to_string();
        let user = json!({"type": "user", "message": {"content": "hi"}}).to_string();
        let text = [main(10_000), main(600_000), side, user, "{not json".to_string()].join("\n");
        assert_eq!(transcript_context(&text), Some((601_002, Some("claude-opus-5-5".into()))));
        assert_eq!(transcript_context("{}\n"), None);
    }

    #[test]
    fn questions_are_answered_through_updated_input() {
        let tool_input = json!({ "questions": [{
            "question": "Which test?", "header": "Test", "multiSelect": false,
            "options": [{ "label": "Live", "description": "Run codex" }, { "label": "Skip" }]
        }] });
        let ev = to_event(&json!({ "hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "tool_input": tool_input }));
        let q = &ev.questions.as_ref().unwrap()[0];
        assert_eq!((q.question.as_str(), q.options.len(), q.options[0].description.as_deref()), ("Which test?", 2, Some("Run codex")));
        assert!(ev.asks_user());

        let answers = [("Which test?".to_string(), "Live".to_string())].into_iter().collect();
        let out = ask_output("AskUserQuestion", &tool_input, &Reply { decision: Some(Decision::Allow), answers: Some(answers), ..Reply::default() }).unwrap();
        let o = &out["hookSpecificOutput"];
        assert_eq!((o["hookEventName"].as_str(), o["permissionDecision"].as_str()), (Some("PreToolUse"), Some("allow")));
        assert_eq!(o["updatedInput"]["answers"]["Which test?"], "Live");
        assert_eq!(o["updatedInput"]["questions"], tool_input["questions"], "pertanyaan asli ikut dikembalikan");
        // No answers: no decision, the agent's UI handles it.
        assert_eq!(ask_output("AskUserQuestion", &tool_input, &Reply::default()), None);

        // Multi-select: the flag goes through to the notch, and the chosen labels are joined back with commas.
        let multi = json!({ "questions": [{
            "question": "Which checks?", "header": "Checks", "multiSelect": true,
            "options": [{ "label": "Lint" }, { "label": "Tests" }, { "label": "Docs" }]
        }] });
        let ev = to_event(&json!({ "hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "tool_input": multi }));
        let q = &ev.questions.as_ref().unwrap()[0];
        assert!(q.multi && q.header.as_deref() == Some("Checks"));
        let answers = [("Which checks?".to_string(), "Lint, Docs".to_string())].into_iter().collect();
        let out = ask_output("AskUserQuestion", &multi, &Reply { decision: Some(Decision::Allow), answers: Some(answers), ..Reply::default() }).unwrap();
        assert_eq!(out["hookSpecificOutput"]["updatedInput"]["answers"]["Which checks?"], "Lint, Docs");
    }

    #[test]
    fn plans_are_approved_or_sent_back() {
        let tool_input = json!({ "plan": "# Plan\n\n- Add widget\n- Write tests", "planFilePath": "p.md" });
        let ev = to_event(&json!({ "hook_event_name": "PreToolUse", "tool_name": "ExitPlanMode", "tool_input": tool_input }));
        assert_eq!(ev.plan.as_deref(), Some("Plan\nAdd widget\nWrite tests"));
        let approve = ask_output("ExitPlanMode", &tool_input, &Reply { decision: Some(Decision::Allow), ..Reply::default() }).unwrap();
        assert_eq!(approve["hookSpecificOutput"]["updatedInput"], tool_input);
        let keep = ask_output("ExitPlanMode", &tool_input, &Reply { decision: Some(Decision::Deny), ..Reply::default() }).unwrap();
        assert_eq!(keep["hookSpecificOutput"]["permissionDecision"], "deny");
    }

    #[test]
    fn permission_json() {
        let allow = permission_output(Decision::Allow);
        assert_eq!(allow["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(permission_output(Decision::Deny)["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
    }
}

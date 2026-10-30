//! Installs and removes notch hooks in Claude Code's settings.json without disturbing anything else.
//! Notch-owned entries are recognized by a command path containing "notch-bridge".

use serde_json::{json, Map, Value};

use notch_protocol::{PERMISSION_WAIT_SECS, QUESTION_WAIT_SECS};

/// Events forwarded to the notch.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
    "SessionEnd",
];

const MARKER: &str = "notch-bridge";

fn is_ours(group: &Value) -> bool {
    group["hooks"].as_array().is_some_and(|hs| hs.iter().any(|h| h["command"].as_str().is_some_and(|c| c.contains(MARKER))))
}

fn handler(exe: &str, event: &str) -> Value {
    // Permissions, questions, and plans (via PreToolUse) wait for the user's answer; the timeout must be
    // longer than the notch's wait, otherwise Claude Code kills the bridge before the answer arrives.
    // The rest only send and exit.
    let timeout = match event {
        "PermissionRequest" => PERMISSION_WAIT_SECS + 15,
        "PreToolUse" => QUESTION_WAIT_SECS + 15,
        _ => 10,
    };
    // `args` runs the exe directly without a shell. Claude Code versions that don't know `args`
    // run `command` alone; the bridge defaults to hook mode, so it still works.
    // Quoted: Claude Code runs the command through a shell, and the path may contain spaces
    // (e.g. C:\Users\John Doe\...).
    json!({ "type": "command", "command": format!("\"{exe}\" hook"), "timeout": timeout })
}

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub events: usize,
    pub statusline: StatusLine,
}

#[derive(Debug, Default, PartialEq)]
pub enum StatusLine {
    #[default]
    Installed,
    /// The user already has their own statusLine; it is not overwritten.
    KeptExisting,
}

/// Adds (or updates) the notch hooks. Safe to call again: old notch-owned entries are replaced.
pub fn install(mut root: Value, exe: &str) -> (Value, Report) {
    let root_obj = root.as_object_mut().expect("settings must be a JSON object");
    let hooks = root_obj.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks = hooks.as_object_mut().unwrap();
    for event in EVENTS {
        let groups = hooks.entry(*event).or_insert_with(|| Value::Array(vec![]));
        if !groups.is_array() {
            *groups = Value::Array(vec![]);
        }
        let groups = groups.as_array_mut().unwrap();
        groups.retain(|g| !is_ours(g));
        groups.push(json!({ "hooks": [handler(exe, event)] }));
    }

    let statusline = match root_obj.get("statusLine") {
        Some(existing) if !existing["command"].as_str().is_some_and(|c| c.contains(MARKER)) => StatusLine::KeptExisting,
        _ => {
            root_obj.insert("statusLine".into(), json!({ "type": "command", "command": format!("\"{exe}\" statusline") }));
            StatusLine::Installed
        }
    };
    (root, Report { events: EVENTS.len(), statusline })
}

/// Removes all notch-owned entries and tidies up objects that became empty.
pub fn uninstall(mut root: Value) -> Value {
    let Some(obj) = root.as_object_mut() else { return root };
    if let Some(hooks) = obj.get_mut("hooks").and_then(Value::as_object_mut) {
        for groups in hooks.values_mut() {
            if let Some(arr) = groups.as_array_mut() {
                arr.retain(|g| !is_ours(g));
            }
        }
        hooks.retain(|_, g| g.as_array().is_none_or(|a| !a.is_empty()));
    }
    if obj.get("hooks").and_then(Value::as_object).is_some_and(Map::is_empty) {
        obj.remove("hooks");
    }
    if obj.get("statusLine").and_then(|s| s["command"].as_str()).is_some_and(|c| c.contains(MARKER)) {
        obj.remove("statusLine");
    }
    root
}

/// Registers the notch MCP server in the `key` object (`mcpServers` for the Claude app and Antigravity,
/// `servers` for VS Code). Other entries are untouched; a "notch" entry that isn't notch's is not
/// overwritten.
pub fn install_mcp_json(mut root: Value, exe: &str, key: &str) -> Value {
    if !root.is_object() {
        root = Value::Object(Map::new());
    }
    let obj = root.as_object_mut().unwrap();
    let servers = obj.entry(key).or_insert_with(|| Value::Object(Map::new()));
    if !servers.is_object() {
        *servers = Value::Object(Map::new());
    }
    let servers = servers.as_object_mut().unwrap();
    if servers.get("notch").is_none_or(is_our_server) {
        // These apps run the exe directly without a shell, so the path needs no quoting.
        let entry = if key == "servers" {
            // VS Code requires the server type to be stated.
            json!({ "type": "stdio", "command": exe, "args": ["mcp"] })
        } else {
            json!({ "command": exe, "args": ["mcp"] })
        };
        servers.insert("notch".into(), entry);
    }
    root
}

pub fn uninstall_mcp_json(mut root: Value, key: &str) -> Value {
    let Some(obj) = root.as_object_mut() else { return root };
    if let Some(servers) = obj.get_mut(key).and_then(Value::as_object_mut) {
        servers.retain(|_, s| !is_our_server(s));
    }
    if obj.get(key).and_then(Value::as_object).is_some_and(Map::is_empty) {
        obj.remove(key);
    }
    root
}

fn is_our_server(s: &Value) -> bool {
    s["command"].as_str().is_some_and(|c| c.contains(MARKER))
}

const CODEX_TABLE: &str = "[mcp_servers.notch]";

/// Line range of the `[mcp_servers.notch]` table (with its sub-tables) in Codex's config.toml, and
/// whether it is owned by notch.
fn codex_section(lines: &[&str]) -> Option<(usize, usize, bool)> {
    let start = lines.iter().position(|l| l.trim() == CODEX_TABLE)?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| {
            let t = l.trim();
            t.starts_with('[') && !t.starts_with("[mcp_servers.notch.")
        })
        .map_or(lines.len(), |i| start + 1 + i);
    let ours = lines[start..end].iter().any(|l| l.contains(MARKER));
    Some((start, end, ours))
}

/// Registers the notch MCP server in Codex's config.toml. Other text is unchanged; a "notch" table owned
/// by the user is not overwritten. No TOML parser: notch writes the table itself, so its shape is fixed.
pub fn install_codex(text: &str, exe: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let kept: Vec<&str> = match codex_section(&lines) {
        Some((_, _, false)) => return text.to_string(),
        Some((start, end, true)) => lines[..start].iter().chain(&lines[end..]).copied().collect(),
        None => lines,
    };
    let mut out = kept.join("\n").trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    // TOML literal string (single quotes): backslashes in the path need no escaping.
    out.push_str(&format!("{CODEX_TABLE}\ncommand = '{exe}'\nargs = [\"mcp\"]\n"));
    out
}

pub fn uninstall_codex(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    match codex_section(&lines) {
        Some((start, end, true)) => {
            let kept: Vec<&str> = lines[..start].iter().chain(&lines[end..]).copied().collect();
            let out = kept.join("\n").trim_end().to_string();
            if out.is_empty() {
                out
            } else {
                out + "\n"
            }
        }
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_json_round_trip() {
        let original = json!({ "preferences": { "x": true }, "mcpServers": { "fs": { "command": "npx", "args": ["fs"] } } });
        let m = "mcpServers";
        let on = install_mcp_json(original.clone(), EXE, m);
        assert_eq!(on["mcpServers"]["notch"]["command"], EXE);
        assert_eq!(on["mcpServers"]["notch"]["args"][0], "mcp");
        assert_eq!(on["mcpServers"]["fs"]["command"], "npx");
        assert_eq!(install_mcp_json(on.clone(), EXE, m), on);
        assert_eq!(uninstall_mcp_json(on, m), original);
        assert_eq!(uninstall_mcp_json(install_mcp_json(json!({}), EXE, m), m), json!({}));
        // Another server named "notch" that belongs to the user is left alone.
        let theirs = json!({ "mcpServers": { "notch": { "command": "other.exe" } } });
        assert_eq!(install_mcp_json(theirs.clone(), EXE, m), theirs);
        // VS Code: a `servers` object with `type`; `inputs` is left alone.
        let vscode = json!({ "inputs": [], "servers": { "gh": { "type": "http", "url": "https://example.test" } } });
        let on = install_mcp_json(vscode.clone(), EXE, "servers");
        assert_eq!(on["servers"]["notch"]["type"], "stdio");
        assert_eq!(on["servers"]["gh"]["url"], "https://example.test");
        assert_eq!(uninstall_mcp_json(on, "servers"), vscode);
    }

    #[test]
    fn codex_round_trip() {
        let original = "model = \"gpt-5\"\n\n[mcp_servers.fs]\ncommand = \"npx\"\n\n[profiles.fast]\nmodel = \"mini\"\n";
        let on = install_codex(original, EXE);
        assert!(on.contains(&format!("[mcp_servers.notch]\ncommand = '{EXE}'\nargs = [\"mcp\"]\n")));
        assert!(on.contains("[profiles.fast]\nmodel = \"mini\""));
        assert_eq!(install_codex(&on, EXE), on, "reinstalling does not duplicate the table");
        assert_eq!(uninstall_codex(&on), original);
        assert_eq!(uninstall_codex(&install_codex("", EXE)), "");
        // A notch table in the middle of the file (with its sub-tables) is removed without touching the tables after it.
        let middle = format!("[mcp_servers.notch]\ncommand = '{EXE}'\n[mcp_servers.notch.env]\nX = \"1\"\n[other]\na = 1\n");
        assert_eq!(uninstall_codex(&middle), "[other]\na = 1\n");
        // A notch table owned by the user is left alone.
        let theirs = "[mcp_servers.notch]\ncommand = \"other.exe\"\n";
        assert_eq!(install_codex(theirs, EXE), theirs);
        assert_eq!(uninstall_codex(theirs), theirs);
    }

    const EXE: &str = "C:/tools/notch/notch-bridge.exe";

    #[test]
    fn install_keeps_existing_settings_and_hooks() {
        let before = json!({
            "modelSettings": { "claude-opus-5-5": { "effortLevel": "high" } },
            "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "lint.sh" } ] } ] }
        });
        let (after, report) = install(before, EXE);
        assert_eq!(report, Report { events: EVENTS.len(), statusline: StatusLine::Installed });
        assert_eq!(after["modelSettings"]["claude-opus-5-5"]["effortLevel"], "high");
        let pre = after["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["hooks"][0]["command"], "lint.sh");
        assert_eq!(after["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"], PERMISSION_WAIT_SECS + 15);
        assert_eq!(pre[1]["hooks"][0]["timeout"], QUESTION_WAIT_SECS + 15, "kartu pertanyaan dan plan menunggu lewat PreToolUse");
        assert_eq!(after["hooks"]["Stop"][0]["hooks"][0]["timeout"], 10);
        // The original key order is preserved.
        assert_eq!(after.as_object().unwrap().keys().next().unwrap(), "modelSettings");
    }

    #[test]
    fn install_is_idempotent_and_respects_custom_statusline() {
        let custom = json!({ "statusLine": { "type": "command", "command": "my-status.sh" } });
        let (once, report) = install(custom, EXE);
        assert_eq!(report.statusline, StatusLine::KeptExisting);
        let (twice, _) = install(once.clone(), EXE);
        assert_eq!(once, twice);
        assert_eq!(twice["statusLine"]["command"], "my-status.sh");
    }

    #[test]
    fn uninstall_restores_original() {
        let original = json!({
            "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "say done" } ] } ] },
            "theme": "dark"
        });
        let (installed, _) = install(original.clone(), EXE);
        assert_eq!(uninstall(installed), original);
        let (bare, _) = install(json!({}), EXE);
        assert_eq!(uninstall(bare), json!({}));
    }
}

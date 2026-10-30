//! notch-bridge: a small link between AI agents and the notch app.
//!
//!   notch-bridge [hook]               called by Claude Code hooks; JSON input from stdin
//!   notch-bridge statusline           called by Claude Code statusLine; prints status for the terminal
//!   notch-bridge mcp                  MCP server (stdio) so agents can use the notch
//!   notch-bridge install-mcp <app>    register that MCP server in another app: claude-app, codex, gemini,
//!                                     cursor (uninstall-mcp removes; install-chat = install-mcp claude-app)
//!   notch-bridge install [settings]   installs hooks into settings.json (default: ~/.claude/settings.json)
//!   notch-bridge uninstall [settings] removes them again
//!
//! If the notch is not running, the hook exits immediately with no output so Claude Code runs normally.

mod claude;
mod install;
mod mcp;
mod owner;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use notch_protocol::{pipe_name, McpApp, Message, Reply};
use serde_json::Value;

/// Hook input can contain full file contents; capped so the bridge doesn't use up memory.
const MAX_INPUT: u64 = 8 * 1024 * 1024;

fn read_stdin() -> Value {
    let mut buf = String::new();
    let _ = std::io::stdin().take(MAX_INPUT).read_to_string(&mut buf);
    serde_json::from_str(&buf).unwrap_or(Value::Null)
}

/// The pipe can be briefly busy when many hooks run in parallel (parallel tools, subagents); retry for
/// about a second so events like permission requests aren't lost. If notch isn't
/// running, the pipe doesn't exist and the bridge gives up immediately.
///
/// The pipe is per user, and the server at its end must be run by the same user; otherwise
/// the bridge sends nothing.
fn connect() -> Option<File> {
    let name = pipe_name(&owner::own_sid()?);
    for _ in 0..50 {
        match OpenOptions::new().read(true).write(true).open(&name) {
            Ok(f) => return owner::same_user(&f).then_some(f),
            Err(e) if e.raw_os_error() == Some(231) => std::thread::sleep(Duration::from_millis(20)), // ERROR_PIPE_BUSY
            Err(_) => return None,
        }
    }
    None
}

fn send(pipe: &mut File, msg: &Message) -> bool {
    let Ok(mut line) = serde_json::to_string(msg) else { return false };
    line.push('\n');
    pipe.write_all(line.as_bytes()).is_ok()
}

/// Only the tail of the transcript; session files can be tens of MB.
fn read_tail(path: &str) -> Option<String> {
    const TAIL: u64 = 512 * 1024;
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    // The first line may be cut off; the transcript parser skips it.
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn hook() -> i32 {
    let input = read_stdin();
    if input.is_null() {
        return 0;
    }
    let mut event = claude::to_event(&input);
    // Context is read on events that mark the end of a turn or tool; enough for the meter in the notch.
    if matches!(event.event.as_str(), "UserPromptSubmit" | "PostToolUse" | "Stop" | "SessionStart") {
        if let Some((tokens, model)) = input.get("transcript_path").and_then(Value::as_str).and_then(read_tail).as_deref().and_then(claude::transcript_context) {
            event.context_tokens = Some(tokens);
            event.model = model;
        }
    }
    // Never asks for approval for something that can't be shown in full.
    if claude::too_long_for_card(&event) {
        return 0;
    }
    let (wait, secs, permission) = (event.wants_reply(), event.wait_secs(), event.is_permission());
    let tool = event.tool.clone().unwrap_or_default();
    let Some(mut pipe) = connect() else { return 0 };
    if !send(&mut pipe, &Message::Hook(event)) || !wait {
        return 0;
    }
    // Safeguard: whatever happens in the notch, don't hold the agent longer than the wait limit.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs + 5));
        std::process::exit(0);
    });
    let mut line = String::new();
    if BufReader::new(pipe).read_line(&mut line).is_ok() {
        if let Ok(reply) = serde_json::from_str::<Reply>(&line) {
            let out = match (permission, reply.decision) {
                (true, Some(d)) => Some(claude::permission_output(d)),
                (true, None) => None,
                (false, _) => claude::ask_output(&tool, input.get("tool_input").unwrap_or(&Value::Null), &reply),
            };
            if let Some(out) = out {
                println!("{out}");
            }
        }
    }
    0
}

fn statusline() -> i32 {
    let input = read_stdin();
    let status = claude::to_status(&input);
    if let Some(mut pipe) = connect() {
        send(&mut pipe, &Message::Status(status.clone()));
    }
    println!("{}", claude::statusline_text(&status));
    0
}

fn settings_path(arg: Option<String>) -> PathBuf {
    arg.map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
        PathBuf::from(home).join(".claude").join("settings.json")
    })
}

/// This exe's path with plain forward slashes, so it is also safe when run via Git Bash.
fn own_exe() -> String {
    std::env::current_exe().map(|p| p.display().to_string().replace('\\', "/")).unwrap_or_else(|_| "notch-bridge".into())
}

/// Edits settings.json via `f`. Returns the backup path if there is one.
fn edit_settings(path: &Path, f: impl FnOnce(Value) -> Value) -> std::io::Result<Option<PathBuf>> {
    edit_text(path, |existing| {
        let root = match existing {
            Some(text) => serde_json::from_str(text).map_err(|e| std::io::Error::other(format!("invalid {}: {e}", path.display())))?,
            None => Value::Object(Default::default()),
        };
        Ok(serde_json::to_string_pretty(&f(root)).map_err(std::io::Error::other)? + "\n")
    })
}

/// Edits a text file via `f` (old content, `None` if it doesn't exist yet). Returns the backup path
/// if there is one.
///
/// The backup is made only once: the switch on the Settings page can install and remove repeatedly,
/// and the most valuable backup is the content from before notch was installed.
fn edit_text(path: &Path, f: impl FnOnce(Option<&str>) -> std::io::Result<String>) -> std::io::Result<Option<PathBuf>> {
    let existing = std::fs::read_to_string(path).ok();
    let out = f(existing.as_deref())?;
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let backup = path.with_extension(format!("{ext}.notch-backup"));
    // Content that already contains notch is not the user's original (e.g. a file notch created itself).
    if let (Some(text), false) = (&existing, backup.exists()) {
        if !text.contains("notch-bridge") {
            std::fs::write(&backup, text)?;
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out)?;
    Ok(backup.exists().then_some(backup))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "hook".into());
    let code = match mode.as_str() {
        "hook" => hook(),
        "statusline" => statusline(),
        "mcp" => mcp::serve(),
        "install" | "uninstall" => {
            let path = settings_path(args.next());
            let exe = own_exe();
            let mut report = None;
            let result = edit_settings(&path, |root| {
                if mode == "install" {
                    let (root, r) = install::install(root, &exe);
                    report = Some(r);
                    root
                } else {
                    install::uninstall(root)
                }
            });
            match (result, report) {
                (Err(e), _) => {
                    eprintln!("Failed to update {}: {e}", path.display());
                    1
                }
                (Ok(backup), Some(r)) => {
                    println!("Installed notch hooks for {} events in {}", r.events, path.display());
                    if r.statusline == install::StatusLine::KeptExisting {
                        println!("Kept your existing statusLine; its quota usage will not show in the notch.");
                    }
                    if let Some(b) = backup {
                        println!("Backup of previous settings: {}", b.display());
                    }
                    0
                }
                (Ok(_), None) => {
                    println!("Removed notch hooks from {}", path.display());
                    0
                }
            }
        }
        "install-mcp" | "uninstall-mcp" | "install-chat" | "uninstall-chat" => {
            // install-chat / uninstall-chat: old name for the Claude app.
            let on = mode.starts_with("install");
            let app = if mode.ends_with("chat") { Some(McpApp::ClaudeApp) } else { args.next().as_deref().and_then(McpApp::from_key) };
            let Some(app) = app else {
                eprintln!("Unknown app. Choices: {}", McpApp::ALL.map(McpApp::key).join(", "));
                std::process::exit(2);
            };
            let Some(path) = args.next().map(PathBuf::from).or_else(|| app.config()) else {
                eprintln!("{} not found.", app.name());
                std::process::exit(1);
            };
            let exe = own_exe();
            let result = if app.is_toml() {
                edit_text(&path, |text| Ok(if on { install::install_codex(text.unwrap_or(""), &exe) } else { install::uninstall_codex(text.unwrap_or("")) }))
            } else {
                edit_settings(&path, |root| if on { install::install_mcp_json(root, &exe, app.servers_key()) } else { install::uninstall_mcp_json(root, app.servers_key()) })
            };
            match result {
                Ok(_) => {
                    println!("Notch MCP server {} {}. Restart {} for it to take effect.", if on { "registered in" } else { "removed from" }, path.display(), app.name());
                    0
                }
                Err(e) => {
                    eprintln!("Failed to update {}: {e}", path.display());
                    1
                }
            }
        }
        _ => {
            eprintln!("Usage: notch-bridge [hook|statusline|mcp|install [settings.json]|uninstall [settings.json]|install-mcp <app> [config]|uninstall-mcp <app> [config]]");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_backup_survives_repeated_edits() {
        let dir = std::env::temp_dir().join(format!("notch-bridge-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{\"theme\": \"dark\"}").unwrap();

        let set = |v: &str| {
            let v = v.to_string();
            edit_settings(&path, move |mut root| {
                root["step"] = Value::String(v);
                root
            })
            .unwrap()
        };
        let backup = set("install").expect("backup created");
        set("uninstall");
        set("install");

        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
        assert_eq!(saved, serde_json::json!({ "theme": "dark" }));
        let current: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(current["step"], "install");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_backup_of_a_file_notch_created() {
        let dir = std::env::temp_dir().join(format!("notch-bridge-own-{}", std::process::id()));
        let path = dir.join("settings.json");
        edit_settings(&path, |root| install::install(root, "notch-bridge.exe").0).unwrap();
        assert_eq!(edit_settings(&path, install::uninstall).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_backup_without_existing_settings() {
        let dir = std::env::temp_dir().join(format!("notch-bridge-fresh-{}", std::process::id()));
        let path = dir.join("settings.json");
        assert_eq!(edit_settings(&path, |root| root).unwrap(), None);
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

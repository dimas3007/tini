//! Connects agents to notch from the Settings page and the Agent tab. For Claude Code, installing the
//! hooks is done by `notch-bridge.exe install|uninstall` (which also backs up settings.json), and the
//! notch MCP server is registered through the `claude` CLI. Other apps (Claude app, Codex, Antigravity, VS Code)
//! are just registered in their MCP config via `install-mcp|uninstall-mcp`.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub use notch_protocol::McpApp;

/// Without a flashing console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn bridge() -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?.with_file_name("notch-bridge.exe");
    p.exists().then_some(p)
}

fn claude_settings() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".claude").join("settings.json"))
}

/// `None` if notch-bridge.exe is not next to notch.exe (cannot be connected).
pub fn claude_connected() -> Option<bool> {
    bridge()?;
    let text = claude_settings().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    Some(text.contains("notch-bridge"))
}

/// The notch MCP server in that app. `None` if the bridge or the app is missing.
pub fn mcp_connected(app: McpApp) -> Option<bool> {
    bridge()?;
    let path = app.config()?;
    let text = std::fs::read_to_string(path).unwrap_or_default();
    Some(text.contains("notch-bridge"))
}

/// Installs or removes the Claude Code hooks; returns `true` if the bridge finishes without error.
/// The notch MCP server is also registered via `claude mcp`, on its own thread because the CLI can be
/// slow and is not required (hooks still work without it).
pub fn set_claude(on: bool) -> bool {
    let ok = run_bridge(&[if on { "install" } else { "uninstall" }]);
    if let Some(exe) = bridge() {
        std::thread::spawn(move || {
            let exe = exe.display().to_string();
            // `cmd /c` so both claude.exe and claude.cmd (npm installs) are found.
            let args: Vec<&str> =
                if on { vec!["/c", "claude", "mcp", "add", "--scope", "user", "notch", "--", &exe, "mcp"] } else { vec!["/c", "claude", "mcp", "remove", "--scope", "user", "notch"] };
            let _ = Command::new("cmd").args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(CREATE_NO_WINDOW).status();
        });
    }
    ok
}

/// Registers or removes the notch MCP server in that app.
pub fn set_mcp(app: McpApp, on: bool) -> bool {
    run_bridge(&[if on { "install-mcp" } else { "uninstall-mcp" }, app.key()])
}

fn run_bridge(args: &[&str]) -> bool {
    let Some(exe) = bridge() else { return false };
    Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .is_ok_and(|s| s.success())
}

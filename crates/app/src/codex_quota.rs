//! Codex quota from its own session logs: every turn, Codex writes a `token_count` event with
//! `rate_limits` to `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (or under `CODEX_HOME`).
//! Only local files are read, with no login token and no network, and only the
//! `rate_limits` lines are parsed. The numbers only change each time Codex is used.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// One Codex quota window (e.g. 5-hour or weekly).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub used_pct: f32,
    /// Window length in minutes: 300 = 5 hours, 10080 = weekly.
    pub minutes: u64,
    /// When it resets (Unix seconds).
    pub resets_at: Option<u64>,
}

/// Tail of each file that is read; the last `token_count` event is always near the end.
const TAIL: u64 = 512 * 1024;
/// How many of the newest session files are checked (the last session may lack `rate_limits`, e.g.
/// when logged in with an API key).
const FILES: usize = 8;

fn sessions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    let base = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".codex")))?;
    Some(base.join("sessions"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The latest quota from any Codex session, with windows past their reset time
/// counted as 0%. `None` if Codex isn't installed or has never logged a quota.
pub fn read() -> Option<Vec<Window>> {
    let mut files = Vec::new();
    newest(&sessions_dir()?, 3, &mut files);
    // An older session that is still running can have newer data: take the one written last.
    let (_, mut windows) = files
        .iter()
        .filter_map(|p| {
            let written = std::fs::metadata(p).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some((written, last_limits(p, written)?))
        })
        .max_by_key(|(written, _)| *written)?;
    let now = now();
    for w in &mut windows {
        if w.resets_at.is_some_and(|t| t <= now) {
            w.used_pct = 0.0;
            w.resets_at = None;
        }
    }
    Some(windows)
}

/// Newest `.jsonl` files under `dir` (year / month / day, then file; names sort by time).
fn newest(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort_unstable_by(|a, b| b.cmp(a));
    for p in paths {
        if out.len() >= FILES {
            return;
        }
        if depth == 0 {
            if p.extension().is_some_and(|e| e == "jsonl") {
                out.push(p);
            }
        } else if p.is_dir() {
            newest(&p, depth - 1, out);
        }
    }
}

fn last_limits(path: &Path, written: u64) -> Option<Vec<Window>> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().filter(|l| l.contains("\"rate_limits\"")).find_map(|l| parse_line(l, written))
}

/// `written`: when the file was last written, for the old format that uses `resets_in_seconds`.
pub fn parse_line(line: &str, written: u64) -> Option<Vec<Window>> {
    let v: Value = serde_json::from_str(line).ok()?;
    let limits = v.pointer("/payload/rate_limits")?;
    let windows: Vec<Window> = ["primary", "secondary"].iter().filter_map(|k| window(limits.get(k)?, written)).collect();
    (!windows.is_empty()).then_some(windows)
}

fn window(w: &Value, written: u64) -> Option<Window> {
    let used_pct = w.get("used_percent")?.as_f64()? as f32;
    let minutes = w.get("window_minutes").and_then(Value::as_u64).unwrap_or(0);
    let resets_at = w.get("resets_at").and_then(Value::as_u64).or_else(|| w.get("resets_in_seconds").and_then(Value::as_u64).map(|s| written + s));
    Some(Window { used_pct, minutes, resets_at })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rate_limits() {
        let line = r#"{"timestamp":"2026-08-14T00:00:00Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","primary":{"used_percent":31.0,"window_minutes":10080,"resets_at":1787201126},"secondary":{"used_percent":4.5,"window_minutes":300,"resets_in_seconds":600},"plan_type":"plus"}}}"#;
        let w = parse_line(line, 1000).unwrap();
        assert_eq!(w[0], Window { used_pct: 31.0, minutes: 10080, resets_at: Some(1787201126) });
        assert_eq!(w[1], Window { used_pct: 4.5, minutes: 300, resets_at: Some(1600) });
    }

    #[test]
    fn ignores_missing_limits() {
        let line = r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":null,"secondary":null}}}"#;
        assert_eq!(parse_line(line, 0), None);
        assert_eq!(parse_line("{\"payload\":{\"rate_limits\":", 0), None);
    }
}

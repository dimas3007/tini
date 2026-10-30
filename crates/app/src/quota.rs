//! Official Claude quota (5-hour and weekly) from the same endpoint as `/usage` in Claude Code.
//!
//! The Claude Code login token is read from `~/.claude/.credentials.json` in memory only, sent only to
//! `api.anthropic.com` (and, when refreshing, to `platform.claude.com`), and never logged.
//! Claude Desktop keeps its own login and does not refresh that file, so without Claude Code running
//! in a terminal the token expires within hours. In that case notch refreshes it itself, exactly
//! like Claude Code: holding the same lock, re-reading the file, then writing the new token to the
//! same file atomically. This endpoint is undocumented, so any failure simply
//! means "no new numbers".
//!
//! The fetch runs on a short-lived thread that finishes immediately; the result is stored, then
//! `WM_APP_QUOTA` is sent to the controller.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

use crate::app::WM_APP_QUOTA;
use crate::watch;

/// A single quota limit: percent used and when it resets (Unix seconds).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Limit {
    pub pct: f32,
    pub resets_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quota {
    pub five_hour: Option<Limit>,
    pub seven_day: Option<Limit>,
}

static LATEST: Mutex<Option<Quota>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);
/// Hash of the refresh token the server rejected; not retried until the user logs in again.
static DEAD_REFRESH: AtomicU64 = AtomicU64::new(0);

/// Claude Code's OAuth client (the same one the CLI uses).
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Claude Code treats the lock as stale after 10 seconds; the whole refresh must finish before that.
const LOCK_STALE: Duration = Duration::from_secs(10);

/// Latest result; read by the controller after `WM_APP_QUOTA`.
pub fn take() -> Option<Quota> {
    LATEST.lock().ok()?.take()
}

/// Fetches the quota on a separate thread, unless the previous fetch has not finished.
pub fn refresh() {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        let quota = fetch();
        BUSY.store(false, Ordering::Release);
        if let Some(q) = quota {
            if let Ok(mut latest) = LATEST.lock() {
                *latest = Some(q);
            }
            watch::post(WM_APP_QUOTA, 0, 0);
        }
    });
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn claude_dir() -> Option<PathBuf> {
    Some(Path::new(&std::env::var_os("USERPROFILE")?).join(".claude"))
}

fn read_credentials(dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(".credentials.json")).ok()?).ok()
}

/// Access token from the file contents, if still valid for at least one more minute.
fn valid_token(root: &Value) -> Option<String> {
    let oauth = root.get("claudeAiOauth")?;
    let expires_ms = oauth.get("expiresAt").and_then(Value::as_u64).unwrap_or(u64::MAX);
    if expires_ms / 1000 <= now_secs() + 60 {
        return None;
    }
    oauth.get("accessToken").and_then(Value::as_str).map(str::to_string)
}

/// Claude Code access token; refreshed first if (nearly) expired.
fn token() -> Option<String> {
    let dir = claude_dir()?;
    let root = read_credentials(&dir)?;
    valid_token(&root).or_else(|| refresh_token(&dir))
}

fn fingerprint(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish() | 1
}

/// Folder lock in the style of `proper-lockfile` used by Claude Code: creating the folder means holding the lock.
/// A folder older than `LOCK_STALE` was left by a dead process, so it may be taken over.
struct DirLock(PathBuf);

impl DirLock {
    fn acquire(path: PathBuf) -> Option<DirLock> {
        for _ in 0..2 {
            match std::fs::create_dir(&path) {
                Ok(()) => return Some(DirLock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
                    if age.is_none_or(|a| a < LOCK_STALE) {
                        return None;
                    }
                    let _ = std::fs::remove_dir(&path);
                }
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// Refreshes the token like Claude Code: take the lock, re-read the file (another process may have just
/// refreshed), exchange the refresh token, then write the result. If the lock is held, just skip;
/// the next fetch tries again.
fn refresh_token(dir: &Path) -> Option<String> {
    let _lock = DirLock::acquire(dir.join(".oauth_refresh.lock"))?;
    // Old lock still honoured by the CLI: `<.claude folder>.lock` next to it.
    let _legacy = DirLock::acquire(dir.with_extension("lock"))?;
    let mut root = read_credentials(dir)?;
    if let Some(t) = valid_token(&root) {
        return Some(t);
    }
    let oauth = root.get("claudeAiOauth")?;
    let refresh = oauth.get("refreshToken").and_then(Value::as_str)?.to_string();
    if DEAD_REFRESH.load(Ordering::Relaxed) == fingerprint(&refresh) {
        return None;
    }
    let scopes = oauth.get("scopes").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default();
    let client_id = oauth.get("clientId").and_then(Value::as_str).unwrap_or(CLIENT_ID);
    let body = serde_json::json!({ "grant_type": "refresh_token", "refresh_token": refresh, "client_id": client_id, "scope": scopes });
    let sent = now_secs();
    let (status, reply) = http(w!("platform.claude.com"), w!("POST"), w!("/v1/oauth/token"), "Content-Type: application/json\r\nAccept: application/json\r\n", Some(body.to_string().as_bytes()))?;
    if status == 400 || status == 401 {
        // Refresh token is dead (e.g. logged out); don't keep retrying.
        DEAD_REFRESH.store(fingerprint(&refresh), Ordering::Relaxed);
        return None;
    }
    if status != 200 {
        return None;
    }
    let reply: Value = serde_json::from_slice(&reply).ok()?;
    let access = reply.get("access_token").and_then(Value::as_str)?.to_string();
    let expires_in = reply.get("expires_in").and_then(Value::as_u64)?;
    let oauth = root.get_mut("claudeAiOauth")?.as_object_mut()?;
    oauth.insert("accessToken".into(), access.clone().into());
    if let Some(r) = reply.get("refresh_token").and_then(Value::as_str) {
        oauth.insert("refreshToken".into(), r.into());
    }
    oauth.insert("expiresAt".into(), ((sent + expires_in) * 1000).into());
    if let Some(scope) = reply.get("scope").and_then(Value::as_str) {
        oauth.insert("scopes".into(), scope.split_whitespace().collect::<Vec<_>>().into());
    }
    write_atomic(&dir.join(".credentials.json"), serde_json::to_string(&root).ok()?.as_bytes()).ok()?;
    Some(access)
}

/// Writes to a temp file in the same folder, then renames it, so readers never see a half-written
/// file. The rename can fail briefly if another process has the file open.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.notch-tmp");
    std::fs::write(&tmp, data)?;
    let mut last = Ok(());
    for _ in 0..20 {
        last = std::fs::rename(&tmp, path);
        if last.is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_file(&tmp);
    last
}

fn fetch() -> Option<Quota> {
    let token = token()?;
    let headers = format!("Authorization: Bearer {token}\r\nanthropic-beta: oauth-2025-04-20\r\nAccept: application/json\r\n");
    let (status, body) = http(w!("api.anthropic.com"), w!("GET"), w!("/api/oauth/usage"), &headers, None)?;
    if status != 200 {
        return None;
    }
    parse(&serde_json::from_slice(&body).ok()?)
}

/// One HTTPS request via WinHTTP (built into Windows, no extra crate): status and body.
/// Short timeout so the token refresh finishes well before the lock is considered stale.
fn http(host: PCWSTR, method: PCWSTR, path: PCWSTR, headers: &str, body: Option<&[u8]>) -> Option<(u32, Vec<u8>)> {
    struct Handle(*mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }
    let open = |h: *mut core::ffi::c_void| (!h.is_null()).then_some(Handle(h));
    unsafe {
        let session = open(WinHttpOpen(w!("Notch"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None, None, 0))?;
        let _ = WinHttpSetTimeouts(session.0, 2000, 2000, 2000, 3000);
        let connect = open(WinHttpConnect(session.0, host, INTERNET_DEFAULT_HTTPS_PORT, 0))?;
        let request = open(WinHttpOpenRequest(connect.0, method, path, None, None, std::ptr::null(), WINHTTP_FLAG_SECURE))?;
        // The token must not follow a redirect to another host.
        let never = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
        let _ = WinHttpSetOption(Some(request.0), WINHTTP_OPTION_REDIRECT_POLICY, Some(&never.to_ne_bytes()));
        let headers: Vec<u16> = headers.encode_utf16().collect();
        let (ptr, len) = body.map_or((std::ptr::null(), 0), |b| (b.as_ptr() as *const core::ffi::c_void, b.len() as u32));
        WinHttpSendRequest(request.0, Some(&headers), Some(ptr), len, len, 0).ok()?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).ok()?;
        let (mut status, mut len) = (0u32, 4u32);
        WinHttpQueryHeaders(request.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, None, Some(&mut status as *mut u32 as *mut _), &mut len, std::ptr::null_mut()).ok()?;
        let mut body = Vec::new();
        loop {
            let mut available = 0u32;
            WinHttpQueryDataAvailable(request.0, &mut available).ok()?;
            if available == 0 || body.len() > 256 * 1024 {
                break;
            }
            let start = body.len();
            body.resize(start + available as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(request.0, body[start..].as_mut_ptr() as *mut _, available, &mut read).ok()?;
            body.truncate(start + read as usize);
        }
        Some((status, body))
    }
}

/// Reads the endpoint's answer. The `limits` list is used first because it is the most common (plans with
/// or without a weekly limit); `five_hour`/`seven_day` as a fallback.
pub fn parse(root: &Value) -> Option<Quota> {
    let limit = |pct: Option<f64>, resets: Option<&str>| pct.map(|p| Limit { pct: p as f32, resets_at: resets.and_then(iso_to_unix) });
    let mut q = Quota::default();
    if let Some(limits) = root.get("limits").and_then(Value::as_array) {
        for l in limits {
            let item = limit(l.get("percent").and_then(Value::as_f64), l.get("resets_at").and_then(Value::as_str));
            match l.get("group").and_then(Value::as_str) {
                Some("session") => q.five_hour = q.five_hour.or(item),
                // There may be more than one (e.g. per model); the fullest one matters.
                Some("weekly") if item.map(|i| i.pct) > q.seven_day.map(|s| s.pct) => q.seven_day = item,
                _ => {}
            }
        }
    }
    let old = |key: &str| root.get(key).and_then(|v| limit(v.get("utilization").and_then(Value::as_f64), v.get("resets_at").and_then(Value::as_str)));
    q.five_hour = q.five_hour.or_else(|| old("five_hour"));
    q.seven_day = q.seven_day.or_else(|| old("seven_day"));
    (q.five_hour.is_some() || q.seven_day.is_some()).then_some(q)
}

/// "2026-10-23T15:39:59.825608+00:00" or "...Z" → Unix seconds.
fn iso_to_unix(s: &str) -> Option<u64> {
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // Time zone: the remainder after the seconds and their fraction.
    let rest = s[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest.as_bytes().first() {
        Some(b'+') | Some(b'-') => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let hh = rest.get(1..3)?.parse::<i64>().ok()?;
            let mm = rest.get(4..6).and_then(|m| m.parse::<i64>().ok()).unwrap_or(0);
            sign * (hh * 3600 + mm * 60)
        }
        _ => 0,
    };
    // Days since 1970-01-01 (days_from_civil algorithm).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + h * 3600 + mi * 60 + se - offset).ok()
}

/// Remaining time until reset, compact: "2h 15m", "40m", "3d 4h".
pub fn until(resets_at: u64) -> Option<String> {
    let left = resets_at.checked_sub(now_secs())?;
    let (d, h, m) = (left / 86400, left % 86400 / 3600, (left % 3600).div_ceil(60).min(59));
    Some(match (d, h) {
        (0, 0) => format!("{}m", m.max(1)),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn iso_dates() {
        assert_eq!(iso_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_to_unix("2026-10-23T15:39:59.825608+00:00"), Some(1_792_769_999));
        assert_eq!(iso_to_unix("2026-10-23T22:39:59+07:00"), Some(1_792_769_999));
        assert_eq!(iso_to_unix("nonsense"), None);
    }

    #[test]
    fn limits_list_wins() {
        let v = json!({
            "five_hour": { "utilization": 99.0, "resets_at": null },
            "seven_day": null,
            "limits": [
                { "group": "session", "percent": 20, "resets_at": "2026-10-23T15:39:59+00:00" },
                { "group": "weekly", "percent": 4, "resets_at": "2026-10-30T17:00:00+00:00" },
                { "group": "weekly", "percent": 31, "resets_at": "2026-10-30T17:00:00+00:00" }
            ]
        });
        let q = parse(&v).unwrap();
        assert_eq!(q.five_hour, Some(Limit { pct: 20.0, resets_at: Some(1_792_769_999) }));
        assert_eq!(q.seven_day.map(|l| l.pct), Some(31.0));
    }

    #[test]
    fn falls_back_to_old_fields() {
        let q = parse(&json!({ "five_hour": { "utilization": 12.5, "resets_at": "1970-01-01T01:00:00Z" } })).unwrap();
        assert_eq!(q.five_hour, Some(Limit { pct: 12.5, resets_at: Some(3600) }));
        assert_eq!(q.seven_day, None);
        assert_eq!(parse(&json!({ "five_hour": null })), None);
    }
}

//! Named pipe server for `notch-bridge`. One thread waits for connections; each connection is handled by
//! its own short thread that reads one JSON line and hands it to the UI thread.
//! For permission requests, that thread waits for the decision from the UI (or a timeout) and then replies.

use std::sync::mpsc::{channel, Sender};
use std::time::Duration;

use notch_protocol::{pipe_name, Message, Reply};
use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
    Win32::Security::{GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER},
    Win32::Storage::FileSystem::{FlushFileBuffers, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
    Win32::System::Pipes::*,
    Win32::System::Threading::{GetCurrentProcess, OpenProcessToken},
};

use crate::app::WM_APP_AGENT;
use crate::watch;

/// Message from the bridge, sent to the UI thread via `WM_APP_AGENT` (`Box<Incoming>` pointer in LPARAM).
pub struct Incoming {
    pub msg: Message,
    /// Present for permissions, questions, and plans. Dropping the sender = no answer; the agent uses
    /// its own dialog.
    pub reply: Option<Sender<Reply>>,
}

const MAX_LINE: usize = 64 * 1024;

struct PipeHandle(HANDLE);
// The pipe HANDLE may be used from another thread; only one thread uses it at a time.
unsafe impl Send for PipeHandle {}

pub fn start() {
    // Debug build only: NOTCH_NO_AGENTS=1 disables the pipe server, so the test notch doesn't show
    // activity from the Claude session that is testing it (e.g. when recording the blink in pill mode).
    if cfg!(debug_assertions) && std::env::var_os("NOTCH_NO_AGENTS").is_some() {
        return;
    }
    std::thread::Builder::new()
        .name("notch-pipe".into())
        .stack_size(64 * 1024)
        .spawn(serve)
        .ok();
}

/// SID of the user running the notch, e.g. "S-1-5-21-…".
fn user_sid() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        // u64 buffer so TOKEN_USER inside it is aligned correctly.
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        got.ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let s = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        s
    }
}

fn serve() {
    // Per-user pipe, only its owner may open it, and the first instance must belong to
    // this notch: if another process already holds that name, the notch doesn't create an instance
    // behind it (and the bridge rejects a server not owned by the same user).
    let Some(sid) = user_sid() else { return };
    let name = HSTRING::from(pipe_name(&sid));
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})"));
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None) }.is_err() {
        return;
    }
    let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
    let mut first = true;
    loop {
        let mode = if first { PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE } else { PIPE_ACCESS_DUPLEX };
        let handle = unsafe {
            CreateNamedPipeW(
                &name,
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                Some(&sa),
            )
        };
        if handle.is_invalid() {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        first = false;
        let connected = unsafe { ConnectNamedPipe(handle, None) }.is_ok() || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if !connected {
            unsafe {
                let _ = CloseHandle(handle);
            }
            continue;
        }
        let pipe = PipeHandle(handle);
        let spawned = std::thread::Builder::new().stack_size(64 * 1024).spawn(move || handle_client(pipe));
        if spawned.is_err() {
            continue;
        }
    }
}

fn read_line(h: HANDLE) -> Option<String> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    while data.len() < MAX_LINE {
        let mut n = 0u32;
        if unsafe { ReadFile(h, Some(&mut buf), Some(&mut n), None) }.is_err() || n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
        if data.contains(&b'\n') {
            break;
        }
    }
    let end = data.iter().position(|&b| b == b'\n').unwrap_or(data.len());
    String::from_utf8(data[..end].to_vec()).ok()
}

fn handle_client(pipe: PipeHandle) {
    let h = pipe.0;
    if let Some(msg) = read_line(h).and_then(|l| serde_json::from_str::<Message>(&l).ok()) {
        let wait = match &msg {
            Message::Hook(ev) if ev.wants_reply() => Some(ev.wait_secs()),
            // MCP calls are always answered, even if only with "already shown".
            Message::Mcp(m) => Some(m.call.wait_secs()),
            _ => None,
        };
        let wants_reply = wait.is_some();
        let (tx, rx) = channel();
        let incoming = Box::new(Incoming { msg, reply: wants_reply.then_some(tx) });
        watch::post(WM_APP_AGENT, 0, Box::into_raw(incoming) as isize);
        if wants_reply {
            let reply = rx.recv_timeout(Duration::from_secs(wait.unwrap_or_default())).unwrap_or_default();
            if let Ok(mut line) = serde_json::to_string(&reply) {
                line.push('\n');
                unsafe {
                    let _ = WriteFile(h, Some(line.as_bytes()), None, None);
                    let _ = FlushFileBuffers(h);
                }
            }
        }
    }
    unsafe {
        let _ = DisconnectNamedPipe(h);
        let _ = CloseHandle(h);
    }
}

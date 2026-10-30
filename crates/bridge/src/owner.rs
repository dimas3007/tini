//! The notch pipe is created per Windows user and can only be opened by its owner. Before sending anything
//! (command, prompt, path), the bridge checks that the server at the pipe's end really runs as the
//! same user; on a PC shared by several accounts, another account cannot impersonate notch
//! and send a fake approval.

use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// A process's TOKEN_USER, in a `u64` buffer so the struct inside is properly aligned.
fn token_user(process: HANDLE) -> Option<Vec<u64>> {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let token = Owned(token);
        let mut len = 0u32;
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
            return None;
        }
        Some(buf)
    }
}

fn sid(buf: &[u64]) -> *mut c_void {
    unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid }
}

/// SID of the user running this process, e.g. "S-1-5-21-…".
pub fn own_sid() -> Option<String> {
    let user = token_user(unsafe { GetCurrentProcess() })?;
    unsafe {
        let mut text: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(sid(&user), &mut text) == 0 || text.is_null() {
            return None;
        }
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        Some(s)
    }
}

/// This pipe's server runs as the same user as the bridge.
pub fn same_user(pipe: &File) -> bool {
    unsafe {
        let mut pid = 0u32;
        if GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) == 0 {
            return false;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let process = Owned(process);
        match (token_user(process.0), token_user(GetCurrentProcess())) {
            (Some(theirs), Some(ours)) => EqualSid(sid(&theirs), sid(&ours)) != 0,
            _ => false,
        }
    }
}

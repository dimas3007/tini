//! Shortcuts from the command bar: Start Menu shortcut index, important folders, and ways to open
//! apps, folders, links, projects in VS Code, or a new Claude Code session. The index is built when the command
//! bar opens (not at startup), and rebuilt at most every few minutes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::System::Com::CoTaskMemFree,
    Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
    Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION},
    Win32::UI::Input::KeyboardAndMouse::*,
    Win32::UI::Shell::*,
    Win32::UI::WindowsAndMessaging::*,
};

use crate::command::{score, url_encode, Entry, Kind};

const CF_UNICODETEXT: u32 = 13;
const REBUILD_AFTER: Duration = Duration::from_secs(10 * 60);
/// Shortcuts that aren't apps people want to open.
const NOISE: [&str; 8] = ["uninstall", "readme", "read me", "help", "documentation", "website", "release notes", "license"];

#[derive(Default)]
pub struct Index {
    entries: Vec<Entry>,
    built: Option<Instant>,
}

impl Index {
    /// Builds the index if missing or stale.
    pub fn refresh(&mut self) {
        if self.built.is_some_and(|t| t.elapsed() < REBUILD_AFTER) {
            return;
        }
        let mut entries = Vec::new();
        for dir in start_menu_dirs() {
            scan(&dir, &mut entries, 0);
        }
        entries.extend(known_folders());
        // The same shortcut can exist for all users and for this user.
        let mut seen = std::collections::HashSet::new();
        entries.retain(|e| seen.insert(e.name.to_lowercase()));
        self.entries = entries;
        self.built = Some(Instant::now());
    }

    /// Best results for `query`, plus `extra` (e.g. agent projects), at most `n`.
    pub fn search(&self, query: &str, extra: &[Entry], n: usize) -> Vec<(u32, Entry)> {
        let mut hits: Vec<(u32, Entry)> = extra
            .iter()
            .chain(self.entries.iter())
            .filter_map(|e| {
                let s = score(&e.name, query);
                (s > 0).then(|| (s, e.clone()))
            })
            .collect();
        // Equal score: stable order (projects first, then index order), so results can be memorized.
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        hits.truncate(n);
        hits
    }
}

fn start_menu_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["APPDATA", "ProgramData"] {
        if let Some(base) = std::env::var_os(var) {
            dirs.push(PathBuf::from(base).join("Microsoft").join("Windows").join("Start Menu").join("Programs"));
        }
    }
    dirs
}

fn scan(dir: &Path, out: &mut Vec<Entry>, depth: usize) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for item in read.flatten() {
        let path = item.path();
        if path.is_dir() {
            if depth < 3 {
                scan(&path, out, depth + 1);
            }
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if !matches!(ext.as_str(), "lnk" | "url" | "appref-ms") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let lower = name.to_lowercase();
        if NOISE.iter().any(|n| lower.contains(n)) {
            continue;
        }
        out.push(Entry { kind: Kind::App, name: name.to_string(), target: path.display().to_string() });
    }
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

pub fn downloads_dir() -> Option<PathBuf> {
    known_folder(&FOLDERID_Downloads)
}

pub fn pictures_dir() -> Option<PathBuf> {
    known_folder(&FOLDERID_Pictures)
}

fn known_folders() -> Vec<Entry> {
    [("Downloads", FOLDERID_Downloads), ("Documents", FOLDERID_Documents), ("Desktop", FOLDERID_Desktop), ("Pictures", FOLDERID_Pictures)]
        .iter()
        .filter_map(|(name, id)| Some(Entry { kind: Kind::Folder, name: name.to_string(), target: known_folder(id)?.display().to_string() }))
        .collect()
}

fn shell(file: &str, params: Option<&str>, dir: Option<&str>) -> bool {
    let file = HSTRING::from(file);
    let params = params.map(HSTRING::from);
    let dir = dir.map(HSTRING::from);
    unsafe {
        let r = ShellExecuteW(
            None,
            w!("open"),
            &file,
            params.as_ref().map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr())),
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            SW_SHOWNORMAL,
        );
        r.0 as isize > 32
    }
}

/// Opens a file, shortcut, folder, or link with its default program.
pub fn open(target: &str) -> bool {
    shell(target, None, None)
}

/// Shows a file in File Explorer with that file selected.
pub fn reveal(path: &str) -> bool {
    shell("explorer.exe", Some(&format!("/select,\"{path}\"")), None)
}

pub fn web_search(query: &str) -> bool {
    open(&format!("https://www.google.com/search?q={}", url_encode(query)))
}

fn code_exe() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("Microsoft VS Code").join("Code.exe")),
        std::env::var_os("ProgramFiles").map(|d| PathBuf::from(d).join("Microsoft VS Code").join("Code.exe")),
    ];
    candidates.into_iter().flatten().find(|p| p.exists())
}

/// Opens a project folder in VS Code; without VS Code, in File Explorer.
pub fn open_project(dir: &str) -> bool {
    match code_exe() {
        Some(code) => shell(&code.display().to_string(), Some(&format!("\"{dir}\"")), None),
        None => open(dir),
    }
}

/// Starts a new Claude Code session in `dir` with `prompt`, in Windows Terminal if available. The session
/// then appears in Live via hooks like any other session.
pub fn start_claude(dir: &str, prompt: &str) -> bool {
    // Double quotes can't pass intact through the command line; semicolons separate wt commands.
    let prompt = prompt.replace('"', "'");
    let wt = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Microsoft").join("WindowsApps").join("wt.exe"));
    match wt.filter(|p| p.exists()) {
        Some(wt) => shell(&wt.display().to_string(), Some(&format!("-d \"{dir}\" claude \"{}\"", prompt.replace(';', "\\;"))), None),
        None => shell("cmd.exe", Some(&format!("/k claude \"{prompt}\"")), Some(dir)),
    }
}

fn process_name(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(process);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

extern "system" fn find_claude(hwnd: HWND, lp: LPARAM) -> BOOL {
    unsafe {
        let visible = IsWindowVisible(hwnd).as_bool() && GetWindow(hwnd, GW_OWNER).is_err();
        if visible && process_name(hwnd).is_some_and(|n| n.to_lowercase().ends_with("\\claude.exe")) {
            *(lp.0 as *mut HWND) = hwnd;
            return false.into();
        }
    }
    true.into()
}

/// The Claude desktop app window, if open.
pub fn claude_window() -> Option<HWND> {
    let mut found = HWND::default();
    unsafe {
        let _ = EnumWindows(Some(find_claude), LPARAM(&mut found as *mut HWND as isize));
    }
    (!found.is_invalid()).then_some(found)
}

/// Brings the Claude desktop app to the front.
pub fn activate_claude() -> bool {
    let Some(hwnd) = claude_window() else { return false };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd).as_bool()
    }
}

/// Copies text to the clipboard.
pub fn copy_text(owner: HWND, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let ok = (|| -> Result<()> {
            EmptyClipboard()?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
            let ptr = GlobalLock(mem) as *mut u16;
            if ptr.is_null() {
                return Err(Error::from_thread());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(mem);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0)))?;
            Ok(())
        })()
        .is_ok();
        let _ = CloseClipboard();
        ok
    }
}

// ---- Deck ----------------------------------------------------------------------------------

/// Keys that need the "extended" flag when sent (arrows, Home/End, Insert/Delete, Win, media).
fn extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0xAD..=0xB3)
}

fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

/// Presses the `keys` combination (modifiers first) then releases it in reverse order, to the window that is
/// currently active.
pub fn send_keys(keys: &[u16]) -> bool {
    let mut inputs = Vec::with_capacity(keys.len() * 2);
    for &k in keys {
        let ext = if extended(k) { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
        inputs.push(key_input(k, 0, ext));
    }
    for &k in keys.iter().rev() {
        let ext = if extended(k) { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
        inputs.push(key_input(k, 0, ext | KEYEVENTF_KEYUP));
    }
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) as usize == inputs.len() }
}

/// Types text into the currently active window (Unicode characters; newline = Enter).
pub fn type_text(text: &str) -> bool {
    let mut inputs = Vec::new();
    for c in text.chars() {
        if c == '\n' {
            inputs.push(key_input(0x0D, 0, KEYBD_EVENT_FLAGS(0)));
            inputs.push(key_input(0x0D, 0, KEYEVENTF_KEYUP));
            continue;
        }
        let mut buf = [0u16; 2];
        for unit in c.encode_utf16(&mut buf) {
            inputs.push(key_input(0, *unit, KEYEVENTF_UNICODE));
            inputs.push(key_input(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
    }
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) as usize == inputs.len() }
}

/// Ctrl, Alt, Shift, or Win is still pressed (e.g. from the Ctrl+Alt+1 hotkey that was just pressed).
pub fn modifiers_down() -> bool {
    [0x10, 0x11, 0x12, 0x5B, 0x5C].iter().any(|&k| unsafe { GetAsyncKeyState(k) } as u16 & 0x8000 != 0)
}

/// Runs a PowerShell command in Windows Terminal (or a PowerShell window) in folder `dir`.
pub fn run_command(dir: &str, command: &str) -> bool {
    let dir = if dir.trim().is_empty() { std::env::var("USERPROFILE").unwrap_or_default() } else { dir.trim().to_string() };
    // Double quotes don't pass intact through the command line; semicolons separate wt commands.
    let command = command.replace('"', "'");
    let wt = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Microsoft").join("WindowsApps").join("wt.exe"));
    match wt.filter(|p| p.exists()) {
        Some(wt) => shell(&wt.display().to_string(), Some(&format!("-d \"{dir}\" powershell -NoExit -Command \"{}\"", command.replace(';', "\\;"))), None),
        None => shell("powershell.exe", Some(&format!("-NoExit -Command \"{command}\"")), Some(&dir)),
    }
}

pub fn lock_screen() -> bool {
    unsafe { windows::Win32::System::Shutdown::LockWorkStation().is_ok() }
}

pub fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Returns focus to `hwnd`. The notch may do this while it is itself the active window.
pub fn focus_window(hwnd: HWND) -> bool {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd).as_bool()
    }
}

/// App icon (or folder, file) at `size` pixels as premultiplied BGRA, from the shell.
pub fn app_icon(target: &str, size: i32) -> Option<crate::chips::Thumb> {
    use windows::Win32::Graphics::Gdi::*;
    let wide = HSTRING::from(target);
    let mut info = SHFILEINFOW::default();
    unsafe {
        let ok = SHGetFileInfoW(&wide, Default::default(), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON);
        if ok == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let icon = info.hIcon;
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let thumb = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok().and_then(|bmp| {
            let old = SelectObject(dc, bmp.into());
            let drawn = DrawIconEx(dc, 0, 0, icon, size, size, 0, None, DI_NORMAL).is_ok();
            let n = (size * size * 4) as usize;
            let mut px = std::slice::from_raw_parts(bits as *const u8, n).to_vec();
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
            // Old icon without alpha: treat as fully opaque.
            let has_alpha = px.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
            for p in px.as_chunks_mut::<4>().0 {
                if !has_alpha {
                    p[3] = 255;
                }
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = (*c as u32 * a / 255) as u8;
                }
            }
            drawn.then_some(crate::chips::Thumb { w: size as u32, h: size as u32, bgra: px })
        });
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        let _ = DestroyIcon(icon);
        thumb
    }
}

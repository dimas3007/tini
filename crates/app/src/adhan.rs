//! Full adhan for the prayer reminder: `adhan.mp3` next to notch.exe, played via Windows' built-in MCI
//! (winmm), so no extra decoder. The file is only open while playing; when it finishes,
//! Windows sends `MM_MCINOTIFY` to the controller, which closes it again.

use std::path::PathBuf;

use windows::core::HSTRING;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Multimedia::mciSendStringW;

const ALIAS: &str = "notch_adhan";

/// The adhan file next to the exe, if present.
pub fn file() -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?.with_file_name("adhan.mp3");
    p.exists().then_some(p)
}

fn send(command: &str, callback: Option<HWND>) -> u32 {
    unsafe { mciSendStringW(&HSTRING::from(command), None, callback) }
}

/// Starts playing the adhan; `false` if the file is missing or Windows refuses (the caller then uses
/// the regular chime). Completion is reported to `notify`.
pub fn play(notify: HWND) -> bool {
    let Some(path) = file() else { return false };
    close();
    if send(&format!("open \"{}\" type mpegvideo alias {ALIAS}", path.display()), None) != 0 {
        return false;
    }
    send(&format!("set {ALIAS} time format milliseconds"), None);
    if send(&format!("play {ALIAS} notify"), Some(notify)) != 0 {
        close();
        return false;
    }
    true
}

/// Currently playing.
pub fn playing() -> bool {
    let mut mode = [0u16; 32];
    let ok = unsafe { mciSendStringW(&HSTRING::from(format!("status {ALIAS} mode")), Some(&mut mode), None) } == 0;
    ok && String::from_utf16_lossy(&mode).starts_with("playing")
}

/// Position and length (milliseconds) of the adhan currently open.
pub fn progress() -> Option<(u32, u32)> {
    let status = |what: &str| -> Option<u32> {
        let mut buf = [0u16; 32];
        let ok = unsafe { mciSendStringW(&HSTRING::from(format!("status {ALIAS} {what}")), Some(&mut buf), None) } == 0;
        ok.then(|| String::from_utf16_lossy(&buf).trim_end_matches('\0').trim().parse().ok()).flatten()
    };
    Some((status("position")?, status("length")?))
}

/// Stops and closes the file (safe to call even when nothing is playing).
pub fn close() {
    send(&format!("close {ALIAS}"), None);
}

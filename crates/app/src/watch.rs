//! Watches Windows without polling: active window changes, active window moves/resizes,
//! and (only while the panel is open) clicks outside the notch or the Esc key.
//!
//! All callbacks here only post messages to the controller window; the logic lives in `app`.

use std::sync::atomic::{AtomicI32, AtomicIsize, Ordering};

use windows::{
    Win32::Foundation::*,
    Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
    Win32::System::LibraryLoader::GetModuleHandleW,
    Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
    Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE,
    Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN},
    Win32::UI::WindowsAndMessaging::*,
};

use crate::app::{WM_APP_ESCAPE, WM_APP_OUTSIDE_CLICK, WM_APP_WINEVENT};
use crate::geometry::Rect;

static CONTROLLER: AtomicIsize = AtomicIsize::new(0);
static ISLAND: [AtomicI32; 4] = [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)];

pub fn set_controller(hwnd: HWND) {
    CONTROLLER.store(hwnd.0 as isize, Ordering::Relaxed);
}

pub fn post(msg: u32, wp: usize, lp: isize) {
    let hwnd = HWND(CONTROLLER.load(Ordering::Relaxed) as *mut _);
    unsafe {
        let _ = PostMessageW(Some(hwnd), msg, WPARAM(wp), LPARAM(lp));
    }
}

/// Read by the mouse hook to decide on an "outside click".
pub fn set_island_rect(r: Rect) {
    for (slot, v) in ISLAND.iter().zip([r.x, r.y, r.w, r.h]) {
        slot.store(v, Ordering::Relaxed);
    }
}

fn island_rect() -> Rect {
    let v: Vec<i32> = ISLAND.iter().map(|a| a.load(Ordering::Relaxed)).collect();
    Rect { x: v[0], y: v[1], w: v[2], h: v[3] }
}

const FLAGS: u32 = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;

#[derive(Default)]
pub struct Watcher {
    global: Vec<HWINEVENTHOOK>,
    /// The location hook covers only the active window's process, so it does not receive events from the whole system.
    location: Option<(u32, HWINEVENTHOOK)>,
    mouse: Option<HHOOK>,
    keyboard: Option<HHOOK>,
}

impl Watcher {
    pub fn start() -> Self {
        let mut w = Self::default();
        unsafe {
            for (min, max) in [
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
                (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            ] {
                let h = SetWinEventHook(min, max, None, Some(win_event), 0, 0, FLAGS);
                if !h.is_invalid() {
                    w.global.push(h);
                }
            }
        }
        w
    }

    pub fn follow(&mut self, fg: HWND) {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
        if pid == 0 || self.location.map(|(p, _)| p) == Some(pid) {
            return;
        }
        unsafe {
            if let Some((_, h)) = self.location.take() {
                let _ = UnhookWinEvent(h);
            }
            let h = SetWinEventHook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE, None, Some(win_event), pid, 0, FLAGS);
            if !h.is_invalid() {
                self.location = Some((pid, h));
            }
        }
    }

    /// The global mouse & keyboard hooks live only while the panel is open.
    pub fn set_panel_hooks(&mut self, on: bool) {
        unsafe {
            if on && self.mouse.is_none() {
                let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
                self.mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0).ok();
                self.keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0).ok();
            } else if !on {
                if let Some(h) = self.mouse.take() {
                    let _ = UnhookWindowsHookEx(h);
                }
                if let Some(h) = self.keyboard.take() {
                    let _ = UnhookWindowsHookEx(h);
                }
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.set_panel_hooks(false);
        unsafe {
            for h in self.global.drain(..) {
                let _ = UnhookWinEvent(h);
            }
            if let Some((_, h)) = self.location.take() {
                let _ = UnhookWinEvent(h);
            }
        }
    }
}

unsafe extern "system" fn win_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, id_child: i32, _: u32, _: u32) {
    if event == EVENT_OBJECT_LOCATIONCHANGE
        && (id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || hwnd != GetForegroundWindow())
    {
        return;
    }
    post(WM_APP_WINEVENT, event as usize, hwnd.0 as isize);
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && matches!(wp.0 as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN) {
        let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
        if !island_rect().contains(info.pt.x, info.pt.y) {
            post(WM_APP_OUTSIDE_CLICK, 0, 0);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

unsafe extern "system" fn keyboard_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && wp.0 as u32 == WM_KEYDOWN {
        let info = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if info.vkCode == VK_ESCAPE.0 as u32 {
            post(WM_APP_ESCAPE, 0, 0);
        }
    }
    // The key is still passed on to other applications.
    CallNextHookEx(None, code, wp, lp)
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// For debug logs: window class + title.
pub fn describe(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    format!("{} '{}'", class_name(hwnd), String::from_utf16_lossy(&buf[..n.max(0) as usize]))
}

/// Shell UI that only passes through briefly (Alt+Tab, Start, taskbar, tray overflow) and the notch's own window.
/// While one of them is active, the notch state is left as before so it does not flicker.
pub fn is_transient(hwnd: HWND) -> bool {
    hwnd.is_invalid()
        || matches!(
            class_name(hwnd).as_str(),
            "ForegroundStaging"
                | "XamlExplorerHostIslandWindow"
                | "MultitaskingViewFrame"
                | "TaskSwitcherWnd"
                | "Windows.UI.Core.CoreWindow"
                | "Shell_TrayWnd"
                | "Shell_SecondaryTrayWnd"
                | "NotifyIconOverflowWindow"
                | "TopLevelWindowForOverflowXamlIsland"
                | "NotchIsland"
                | "NotchController"
                | "NotchInput"
        )
}

/// Empty desktop: no window covers the notch.
pub fn is_desktop(hwnd: HWND) -> bool {
    matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW")
}

/// Windows that do not determine which monitor the notch is shown on.
pub fn is_ignored(hwnd: HWND) -> bool {
    is_transient(hwnd) || is_desktop(hwnd)
}

/// Bounds of the visible window (without the invisible border Windows 10/11 windows have).
fn visible_bounds(hwnd: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32)
            .ok()?;
    }
    Some(Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
}

/// (fullscreen, shy) for the active window relative to the notch's monitor and the pill area.
pub fn classify(fg: HWND, monitor: &Rect, zone: &Rect) -> (bool, bool) {
    unsafe {
        if is_ignored(fg) || !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() {
            return (false, false);
        }
    }
    let Some(b) = visible_bounds(fg) else { return (false, false) };
    let fullscreen = b.covers(monitor);
    (fullscreen, !fullscreen && b.intersects(zone))
}

/// Windows presentation mode or exclusive fullscreen Direct3D game.
pub fn presentation_mode() -> bool {
    matches!(unsafe { SHQueryUserNotificationState() }, Ok(s) if s == QUNS_PRESENTATION_MODE || s == QUNS_RUNNING_D3D_FULL_SCREEN)
}

pub fn foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}

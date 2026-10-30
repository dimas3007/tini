//! Monitors and DPI.

use windows::{
    core::BOOL,
    Win32::Foundation::{HWND, LPARAM, POINT, RECT},
    Win32::Graphics::Gdi::*,
    Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
};

use crate::geometry::{Placement, Rect};

pub fn monitor_of(hwnd: HWND) -> HMONITOR {
    unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
}

pub fn primary() -> HMONITOR {
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
}

/// Entire monitor area (including the taskbar). `None` if the monitor has been unplugged.
pub fn bounds(monitor: HMONITOR) -> Option<Rect> {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool().then(|| {
        let r = info.rcMonitor;
        Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
    })
}

pub fn scale(monitor: HMONITOR) -> f32 {
    let (mut x, mut y) = (96u32, 96u32);
    match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
        Ok(()) => x as f32 / 96.0,
        Err(_) => 1.0,
    }
}

pub fn placement(monitor: HMONITOR) -> Option<Placement> {
    Some(Placement { monitor: bounds(monitor)?, scale: scale(monitor) })
}

/// Monitor at a screen point (the nearest one if outside all monitors).
pub fn at(x: i32, y: i32) -> HMONITOR {
    unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) }
}

/// Monitor device name, e.g. `\\.\DISPLAY2`; stable as long as the monitor stays connected.
pub fn device_name(monitor: HMONITOR) -> String {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
        return String::new();
    }
    let end = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
    String::from_utf16_lossy(&info.szDevice[..end])
}

extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
    unsafe { (*(lp.0 as *mut Vec<HMONITOR>)).push(monitor) };
    true.into()
}

/// Monitor with that device name, if still connected.
pub fn by_device(name: &str) -> Option<HMONITOR> {
    if name.is_empty() {
        return None;
    }
    let mut all: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut all as *mut Vec<HMONITOR> as isize));
    }
    all.into_iter().find(|m| device_name(*m) == name)
}

/// Human-friendly name: "main display" or "display 2".
pub fn label(monitor: HMONITOR) -> String {
    if monitor == primary() {
        return "the main display".into();
    }
    let name = device_name(monitor);
    match name.trim_start_matches(r"\\.\DISPLAY").parse::<u32>() {
        Ok(n) => format!("display {n}"),
        Err(_) => "this display".into(),
    }
}

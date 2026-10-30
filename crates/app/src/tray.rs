//! Tray icon and its menu.

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Graphics::Gdi::*,
    Win32::UI::Shell::*,
    Win32::UI::WindowsAndMessaging::*,
};

use crate::app::WM_APP_TRAY;
use crate::settings::MonitorMode;

const ICON_ID: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCmd {
    Toggle = 1,
    TestGlance,
    MonitorFollow,
    MonitorStay,
    Autostart,
    Quit,
}

const COMMANDS: [MenuCmd; 6] =
    [MenuCmd::Toggle, MenuCmd::TestGlance, MenuCmd::MonitorFollow, MenuCmd::MonitorStay, MenuCmd::Autostart, MenuCmd::Quit];

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
}

impl Tray {
    pub fn add(hwnd: HWND) -> Result<Self> {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
        let tray = Self { hwnd, icon: make_icon(size)? };
        tray.readd();
        Ok(tray)
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        for (dst, src) in nid.szTip.iter_mut().zip("Notch".encode_utf16()) {
            *dst = src;
        }
        nid
    }

    /// Also called when Explorer restarts (TaskbarCreated message).
    pub fn readd(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.data());
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            let _ = DestroyIcon(self.icon);
        }
    }
}

/// Shows the menu and returns the choice. Do not call while holding app state:
/// the menu runs a modal loop that still processes other messages.
/// `hotkey_ok = false`: Ctrl+Alt+N is already used by another app.
pub fn show_menu(owner: HWND, monitor: MonitorMode, autostart: bool, hotkey_ok: bool) -> Option<MenuCmd> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let sub = CreatePopupMenu().ok()?;
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let toggle = if hotkey_ok { w!("Open / close notch\tCtrl+Alt+N") } else { w!("Open / close notch\t(Ctrl+Alt+N in use)") };
        let _ = AppendMenuW(menu, MF_STRING, MenuCmd::Toggle as usize, toggle);
        let _ = AppendMenuW(menu, MF_STRING, MenuCmd::TestGlance as usize, w!("Test glance"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(
            sub,
            MF_STRING | check(monitor == MonitorMode::Stay),
            MenuCmd::MonitorStay as usize,
            w!("Stay where I put it (drag to move)"),
        );
        let _ = AppendMenuW(
            sub,
            MF_STRING | check(monitor == MonitorMode::Follow),
            MenuCmd::MonitorFollow as usize,
            w!("Follow active window"),
        );
        let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Monitor"));
        let _ = AppendMenuW(menu, MF_STRING | check(autostart), MenuCmd::Autostart as usize, w!("Start with Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MenuCmd::Quit as usize, w!("Quit"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Without this the menu does not close when the user clicks elsewhere.
        let _ = SetForegroundWindow(owner);
        let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, owner, None);
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        COMMANDS.iter().copied().find(|c| *c as i32 == picked.0)
    }
}

/// Silver pill icon with a pair of Nochi eyes, drawn per pixel so no .ico file or resource compiler is needed.
fn make_icon(size: i32) -> Result<HICON> {
    unsafe {
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
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);

        let n = size as f32;
        let (w, h) = (n * 0.86, n * 0.42);
        let r = h / 2.0;
        let (cx, cy) = (n / 2.0, n / 2.0);
        for y in 0..size {
            for x in 0..size {
                // Signed distance to the rounded rect, for smooth edges.
                let px = (x as f32 + 0.5 - cx).abs() - (w / 2.0 - r);
                let py = (y as f32 + 0.5 - cy).abs() - (h / 2.0 - r);
                let outside = (px.max(0.0).powi(2) + py.max(0.0).powi(2)).sqrt() + px.max(py).min(0.0) - r;
                let alpha = (0.5 - outside).clamp(0.0, 1.0);
                let a = (alpha * 255.0) as u32;
                // Two dark upright capsule-shaped eyes in the middle of the pill.
                let (ex, ey) = ((x as f32 + 0.5 - cx).abs() - n * 0.13, y as f32 + 0.5 - cy);
                let eye = ex.abs() <= n * 0.045 && ey.abs() <= n * 0.1;
                let rgb = if eye { 0x1C1C1E } else { 0xE5E5EA };
                pixels[(y * size + x) as usize] = (a << 24) | rgb;
            }
        }

        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info);
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

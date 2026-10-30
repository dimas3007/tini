//! Task input field: a small popup holding the built-in Windows EDIT control, placed right above the
//! "Add task" row in the panel. The built-in control is used so typing, IME, and Win+H dictation work immediately.
//! Enter and Esc are caught in the message loop (`main.rs`) before they reach the control, so it does not beep.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND},
    Win32::Graphics::Gdi::*,
    Win32::UI::Input::KeyboardAndMouse::SetFocus,
    Win32::UI::WindowsAndMessaging::*,
};

use crate::app::WM_APP_INPUT_CHANGED;
use crate::geometry::Rect;
use crate::watch;

const CLASS: PCWSTR = w!("NotchInput");
const EN_CHANGE: u32 = 0x0300;
// The EDIT control constants live in the large `Win32_UI_Controls` feature; only the values are needed.
const ES_AUTOHSCROLL: u32 = 0x0080;
const EM_SETLIMITTEXT: u32 = 0x00C5;
const EM_SETSEL: u32 = 0x00B1;
/// theme::FIELD (#161618) and theme::LABEL (#F5F5F7) in COLORREF format (0x00BBGGRR).
const FILL: COLORREF = COLORREF(0x0018_1616);
const TEXT: COLORREF = COLORREF(0x00F7_F5F5);
/// No DWM border around the popup.
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

static EDIT: AtomicIsize = AtomicIsize::new(0);
static BRUSH: AtomicIsize = AtomicIsize::new(0);

/// For the message loop: whether this message belongs to the input field's EDIT control.
pub fn is_edit(hwnd: HWND) -> bool {
    !hwnd.is_invalid() && hwnd.0 as isize == EDIT.load(Ordering::Relaxed)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CTLCOLOREDIT => {
                let hdc = HDC(wp.0 as *mut _);
                SetTextColor(hdc, TEXT);
                SetBkColor(hdc, FILL);
                LRESULT(BRUSH.load(Ordering::Relaxed))
            }
            WM_COMMAND if (wp.0 >> 16) as u32 == EN_CHANGE => {
                watch::post(WM_APP_INPUT_CHANGED, 0, 0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

pub struct Input {
    popup: HWND,
    edit: HWND,
    brush: HBRUSH,
    font: Option<HFONT>,
    font_scale: f32,
    visible: bool,
    compact: bool,
}

impl Input {
    pub fn create(instance: HINSTANCE) -> Result<Self> {
        unsafe {
            let brush = CreateSolidBrush(FILL);
            BRUSH.store(brush.0 as isize, Ordering::Relaxed);
            let wc = WNDCLASSW {
                hInstance: instance,
                lpszClassName: CLASS,
                lpfnWndProc: Some(wndproc),
                hbrBackground: brush,
                hCursor: LoadCursorW(None, IDC_IBEAM)?,
                ..Default::default()
            };
            RegisterClassW(&wc);
            let popup = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS,
                w!("Add a task"),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                Some(instance),
                None,
            )?;
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!(""),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL),
                0,
                0,
                10,
                10,
                Some(popup),
                None,
                Some(instance),
                None,
            )?;
            EDIT.store(edit.0 as isize, Ordering::Relaxed);
            SendMessageW(edit, EM_SETLIMITTEXT, Some(WPARAM(500)), None);
            let round = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(popup, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const _, 4);
            let _ = DwmSetWindowAttribute(popup, DWMWA_BORDER_COLOR, &DWMWA_COLOR_NONE as *const _ as *const _, 4);
            Ok(Self { popup, edit, brush, font: None, font_scale: 0.0, visible: false, compact: false })
        }
    }


    fn set_font(&mut self, scale: f32) {
        if (self.font_scale - scale).abs() < f32::EPSILON {
            return;
        }
        unsafe {
            let font = CreateFontW(
                -(12.5 * scale).round() as i32,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                DEFAULT_PITCH.0 as u32,
                w!("Segoe UI Variable Text"),
            );
            SendMessageW(self.edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            if let Some(old) = self.font.replace(font) {
                let _ = DeleteObject(old.into());
            }
        }
        self.font_scale = scale;
    }

    /// Places the popup at `r` (screen pixels). With `focus`, the popup is activated and the text caret enters the field.
    /// `compact`: the field is still empty and its hint is drawn by the notch beside it, so the popup is only
    /// as wide as the text caret.
    pub fn place(&mut self, r: Rect, scale: f32, focus: bool, compact: bool) {
        self.set_font(scale);
        unsafe {
            let flags = if focus { SWP_SHOWWINDOW } else { SWP_SHOWWINDOW | SWP_NOACTIVATE };
            let w = if compact { ((3.0 * scale).round() as i32).max(3) } else { r.w };
            let _ = SetWindowPos(self.popup, Some(HWND_TOPMOST), r.x, r.y, w, r.h, flags);
            let pad = if compact { 0 } else { (2.0 * scale).round() as i32 };
            let eh = (17.0 * scale).round() as i32;
            let _ = MoveWindow(self.edit, pad, (r.h - eh) / 2, w - 2 * pad, eh, true);
            if self.compact && !compact {
                // The first letter is typed while the field is still narrow, so the control has already scrolled; scroll back
                // to the start, then put the caret at the end.
                let end = GetWindowTextLengthW(self.edit).max(0) as usize;
                SendMessageW(self.edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(0)));
                SendMessageW(self.edit, EM_SETSEL, Some(WPARAM(end)), Some(LPARAM(end as isize)));
            }
            self.compact = compact;
            if focus {
                let _ = SetForegroundWindow(self.popup);
                let _ = SetFocus(Some(self.edit));
            }
        }
        self.visible = true;
    }

    /// Rises back above the notch after the notch reasserts its position as the topmost window.
    pub fn raise(&self) {
        if self.visible {
            unsafe {
                let _ = SetWindowPos(self.popup, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(self.popup, SW_HIDE);
            }
            self.visible = false;
        }
    }

    pub fn text(&self) -> String {
        unsafe {
            let len = GetWindowTextLengthW(self.edit).max(0) as usize;
            let mut buf = vec![0u16; len + 1];
            let n = GetWindowTextW(self.edit, &mut buf).max(0) as usize;
            String::from_utf16_lossy(&buf[..n])
        }
    }

    pub fn clear(&self) {
        unsafe {
            let _ = SetWindowTextW(self.edit, w!(""));
        }
    }

    /// Fills the field (e.g. from `notch.exe --command "<text>"`); EN_CHANGE is sent too.
    pub fn set_text(&self, text: &str) {
        unsafe {
            let _ = SetWindowTextW(self.edit, &HSTRING::from(text));
        }
    }

}

impl Drop for Input {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.popup);
            if let Some(font) = self.font.take() {
                let _ = DeleteObject(font.into());
            }
            let _ = DeleteObject(self.brush.into());
        }
    }
}

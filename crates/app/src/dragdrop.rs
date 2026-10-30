//! Notch as a drop target: apps, shortcuts, files, folders, or links dragged onto the notch become
//! deck buttons. OLE callbacks run on the UI thread; the payload is forwarded to the controller via
//! `WM_APP_DRAG` so app state is not changed in the middle of a drag.
//!
//! Notch only creates links: the requested effect is always Link (or Copy if the source does not
//! allow Link), never Move, so the original file is never moved.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::System::Com::*,
    Win32::System::DataExchange::RegisterClipboardFormatW,
    Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock},
    Win32::System::Ole::*,
    Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
    Win32::UI::Shell::{DragQueryFileW, HDROP},
};

use crate::app::WM_APP_DRAG;
use crate::watch;

/// WPARAM of `WM_APP_DRAG`. For `ENTER` and `DROP`, LPARAM = `Box<Vec<Dropped>>`.
pub const ENTER: usize = 1;
pub const LEAVE: usize = 2;
pub const DROP: usize = 3;

const CF_HDROP: u16 = 15;

/// What was dragged onto the notch.
#[derive(Clone, Debug, PartialEq)]
pub enum Dropped {
    /// File, folder, or shortcut.
    Path(String),
    Link(String),
}

/// Effect used for this drag (0 = not accepted).
static EFFECT: AtomicU32 = AtomicU32::new(0);

fn format(cf: u16) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

fn read(data: &IDataObject) -> Vec<Dropped> {
    let mut out = Vec::new();
    unsafe {
        if let Ok(mut medium) = data.GetData(&format(CF_HDROP)) {
            let hdrop = HDROP(medium.u.hGlobal.0);
            let n = DragQueryFileW(hdrop, u32::MAX, None);
            for i in 0..n.min(20) {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, i, Some(&mut buf));
                out.push(Dropped::Path(String::from_utf16_lossy(&buf[..len])));
            }
            ReleaseStgMedium(&mut medium);
            return out;
        }
        let url = RegisterClipboardFormatW(w!("UniformResourceLocatorW")) as u16;
        if let Ok(mut medium) = data.GetData(&format(url)) {
            let mem = medium.u.hGlobal;
            let ptr = GlobalLock(mem) as *const u16;
            if !ptr.is_null() {
                let units = (GlobalSize(mem) / 2).min(4096);
                let wide = std::slice::from_raw_parts(ptr, units);
                let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
                let link = String::from_utf16_lossy(&wide[..end]).trim().to_string();
                if link.starts_with("http://") || link.starts_with("https://") {
                    out.push(Dropped::Link(link));
                }
                let _ = GlobalUnlock(mem);
            }
            ReleaseStgMedium(&mut medium);
        }
    }
    out
}

/// Link if the source allows it, otherwise Copy; never Move.
fn choose(allowed: DROPEFFECT, items: &[Dropped]) -> DROPEFFECT {
    if items.is_empty() {
        DROPEFFECT_NONE
    } else if allowed.0 & DROPEFFECT_LINK.0 != 0 {
        DROPEFFECT_LINK
    } else if allowed.0 & DROPEFFECT_COPY.0 != 0 {
        DROPEFFECT_COPY
    } else {
        DROPEFFECT_NONE
    }
}

#[implement(IDropTarget)]
struct Target;

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let items = data.ok().map(read).unwrap_or_default();
        let chosen = choose(unsafe { *effect }, &items);
        EFFECT.store(chosen.0, Ordering::Relaxed);
        unsafe { *effect = chosen };
        if chosen != DROPEFFECT_NONE {
            watch::post(WM_APP_DRAG, ENTER, Box::into_raw(Box::new(items)) as isize);
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        unsafe { *effect = DROPEFFECT(EFFECT.load(Ordering::Relaxed)) };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        if EFFECT.swap(0, Ordering::Relaxed) != 0 {
            watch::post(WM_APP_DRAG, LEAVE, 0);
        }
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let items = data.ok().map(read).unwrap_or_default();
        let chosen = choose(unsafe { *effect }, &items);
        EFFECT.store(0, Ordering::Relaxed);
        unsafe { *effect = chosen };
        if chosen != DROPEFFECT_NONE {
            watch::post(WM_APP_DRAG, DROP, Box::into_raw(Box::new(items)) as isize);
        } else {
            watch::post(WM_APP_DRAG, LEAVE, 0);
        }
        Ok(())
    }
}

/// Registers the notch window as a drop target.
pub fn register(hwnd: HWND) {
    unsafe {
        // COM is already STA (DispatcherQueue); OLE still needs initializing for drag & drop.
        let _ = OleInitialize(None);
        let target: IDropTarget = Target.into();
        if RegisterDragDrop(hwnd, &target).is_err() {
            eprintln!("[notch] notch cannot accept drops");
        }
    }
}

pub fn revoke(hwnd: HWND) {
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}

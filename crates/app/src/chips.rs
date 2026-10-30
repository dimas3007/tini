//! Smart chip sources: clipboard (copied links and images), finished downloads, and screen
//! unlock (for the greeting). All based on Windows events; no polling.
//!
//! Clipboard contents are only read if they are a link or image, and not at all if the copying app
//! marks them secret (e.g. a password manager).

use std::path::PathBuf;

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Graphics::Imaging::*,
    Win32::Storage::FileSystem::*,
    Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
    Win32::System::DataExchange::*,
    Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
    Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION},
};

use crate::app::WM_APP_DOWNLOAD;
use crate::{command, launcher, watch};

pub const WM_CLIPBOARDUPDATE: u32 = 0x031D;
pub const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
pub const WTS_SESSION_LOCK: usize = 7;
pub const WTS_SESSION_UNLOCK: usize = 8;

const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
/// Largest thumbnail (pixels); displayed at 46 × 30 logical.
const THUMB_W: u32 = 96;
const THUMB_H: u32 = 64;
/// Suffixes of the browser's temporary file while downloading.
const PARTIAL: [&str; 6] = [".crdownload", ".part", ".partial", ".download", ".opdownload", ".tmp"];

/// Starts listening to the clipboard, unlock, and the Downloads folder.
pub fn listen(hwnd: HWND) {
    unsafe {
        let _ = AddClipboardFormatListener(hwnd);
        let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
    }
    if let Some(dir) = launcher::downloads_dir() {
        let _ = std::thread::Builder::new().name("notch-downloads".into()).stack_size(64 * 1024).spawn(move || watch_downloads(dir));
    }
}

pub fn stop(hwnd: HWND) {
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
        let _ = WTSUnRegisterSessionNotification(hwnd);
    }
}

/// Small BGRA image (premultiplied, full alpha), rows from the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Thumb {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Clip {
    Link { url: String, host: String, title: String },
    Image { w: u32, h: u32, thumb: Thumb },
}

fn registered(name: PCWSTR) -> u32 {
    unsafe { RegisterClipboardFormatW(name) }
}

/// Clipboard contents worth turning into a chip, or `None`.
pub fn read_clipboard(owner: HWND) -> Option<Clip> {
    unsafe {
        // Password managers mark their contents so clipboard monitors don't read them.
        let secret = registered(w!("ExcludeClipboardContentFromMonitorProcessing"));
        if IsClipboardFormatAvailable(secret).is_ok() {
            return None;
        }
        // The clipboard may be briefly held by another app.
        let mut opened = false;
        for _ in 0..4 {
            if OpenClipboard(Some(owner)).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        if !opened {
            return None;
        }
        let history = registered(w!("CanIncludeInClipboardHistory"));
        let private = IsClipboardFormatAvailable(history).is_ok() && read_u32(history) == Some(0);
        let clip = if private {
            None
        } else if IsClipboardFormatAvailable(CF_DIB).is_ok() {
            with_bytes(CF_DIB, |b| {
                let d = Dib::parse(b)?;
                Some(Clip::Image { w: d.w as u32, h: d.h as u32, thumb: thumbnail(b, &d) })
            })
        } else if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
            read_text().and_then(|t| {
                let (host, title) = command::link(&t)?;
                Some(Clip::Link { url: t.trim().to_string(), host, title })
            })
        } else {
            None
        };
        let _ = CloseClipboard();
        clip
    }
}

/// Newly copied text for the clipboard history, with the same safeguards: content flagged by a
/// password manager or excluded from Windows history is not read. Very long text is skipped.
pub fn read_plain_text(owner: HWND) -> Option<String> {
    const MAX: usize = 32 * 1024;
    unsafe {
        let secret = registered(w!("ExcludeClipboardContentFromMonitorProcessing"));
        if IsClipboardFormatAvailable(secret).is_ok() || IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return None;
        }
        let mut opened = false;
        for _ in 0..4 {
            if OpenClipboard(Some(owner)).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        if !opened {
            return None;
        }
        let history = registered(w!("CanIncludeInClipboardHistory"));
        let private = IsClipboardFormatAvailable(history).is_ok() && read_u32(history) == Some(0);
        let text = if private {
            None
        } else {
            with_bytes(CF_UNICODETEXT, |b| {
                let wide: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&c| c != 0).take(MAX + 1).collect();
                (wide.len() <= MAX).then(|| String::from_utf16_lossy(&wide))
            })
        };
        let _ = CloseClipboard();
        text.filter(|t| !t.trim().is_empty())
    }
}

/// Reads the contents of an already-opened clipboard format in place, without copying it (a full-screen
/// image can be tens of MB).
unsafe fn with_bytes<T>(format: u32, f: impl FnOnce(&[u8]) -> Option<T>) -> Option<T> {
    let h = GetClipboardData(format).ok()?;
    let mem = HGLOBAL(h.0);
    let size = GlobalSize(mem);
    let ptr = GlobalLock(mem) as *const u8;
    if ptr.is_null() {
        return None;
    }
    let out = if size == 0 { None } else { f(std::slice::from_raw_parts(ptr, size)) };
    let _ = GlobalUnlock(mem);
    out
}

unsafe fn read_u32(format: u32) -> Option<u32> {
    with_bytes(format, |b| Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?)))
}

unsafe fn read_text() -> Option<String> {
    with_bytes(CF_UNICODETEXT, |b| {
        let wide: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&c| c != 0).take(2049).collect();
        // Long text isn't a link.
        (wide.len() <= 2048).then(|| String::from_utf16_lossy(&wide))
    })
}

fn le32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

/// Pixel layout of a 24 or 32-bit DIB.
struct Dib {
    w: usize,
    h: usize,
    bottom_up: bool,
    bytes_pp: usize,
    offset: usize,
    stride: usize,
}

impl Dib {
    fn parse(b: &[u8]) -> Option<Dib> {
        let header = le32(b, 0)? as usize;
        let w = le32(b, 4)?;
        let h = le32(b, 8)?;
        let bpp = le16(b, 14)? as usize;
        let compression = le32(b, 16)?;
        let colors = le32(b, 32)? as usize;
        // Clipboard content comes from any app: header size and color count are limited to valid
        // values, and all computations are checked, so corrupt data can't make the app exit.
        if w <= 0 || h == 0 || !matches!(bpp, 24 | 32) || !matches!(compression, 0 | 3) || w > 16384 || h.abs() > 16384 {
            return None;
        }
        if !matches!(header, 40 | 52 | 56 | 108 | 124) || colors > 256 {
            return None;
        }
        // BI_BITFIELDS with a 40-byte header followed by three color masks.
        let masks = if compression == 3 && header == 40 { 12 } else { 0 };
        let (w, rows) = (w as usize, h.unsigned_abs() as usize);
        let offset = header.checked_add(masks)?.checked_add(colors.checked_mul(4)?)?;
        let stride = w.checked_mul(bpp)?.div_ceil(32).checked_mul(4)?;
        let d = Dib { w, h: rows, bottom_up: h > 0, bytes_pp: bpp / 8, offset, stride };
        (b.len() >= offset.checked_add(stride.checked_mul(rows)?)?).then_some(d)
    }

    /// B, G, R of pixel (x, y), with y from the top.
    fn pixel<'a>(&self, b: &'a [u8], x: usize, y: usize) -> &'a [u8] {
        let row = if self.bottom_up { self.h - 1 - y } else { y };
        let o = self.offset + row * self.stride + x * self.bytes_pp;
        &b[o..o + 3]
    }
}

/// DIB as (width, height, BGRA rows from the top).
fn dib_to_bgra(b: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let d = Dib::parse(b)?;
    let mut out = vec![0u8; d.w * d.h * 4];
    for y in 0..d.h {
        for x in 0..d.w {
            let o = (y * d.w + x) * 4;
            out[o..o + 3].copy_from_slice(d.pixel(b, x, y));
            out[o + 3] = 255;
        }
    }
    Some((d.w as u32, d.h as u32, out))
}

/// Thumbnail directly from the DIB: box average, at most 8 × 8 samples per pixel, so
/// large images stay fast and need no copying.
fn thumbnail(b: &[u8], d: &Dib) -> Thumb {
    let (w, h) = (d.w, d.h);
    let k = (THUMB_W as f32 / w as f32).min(THUMB_H as f32 / h as f32).min(1.0);
    let (tw, th) = (((w as f32 * k).round() as usize).max(1), ((h as f32 * k).round() as usize).max(1));
    let mut out = vec![0u8; tw * th * 4];
    for ty in 0..th {
        let (y0, y1) = (ty * h / th, ((ty + 1) * h / th).max(ty * h / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w / tw).max(tx * w / tw + 1));
            let (sy, sx) = (((y1 - y0) / 8).max(1), ((x1 - x0) / 8).max(1));
            let mut sum = [0u32; 3];
            let mut n = 0u32;
            for y in (y0..y1).step_by(sy) {
                for x in (x0..x1).step_by(sx) {
                    for (acc, &c) in sum.iter_mut().zip(d.pixel(b, x, y)) {
                        *acc += c as u32;
                    }
                    n += 1;
                }
            }
            let o = (ty * tw + tx) * 4;
            for (dst, acc) in out[o..o + 3].iter_mut().zip(sum) {
                *dst = (acc / n.max(1)) as u8;
            }
            out[o + 3] = 255;
        }
    }
    Thumb { w: tw as u32, h: th as u32, bgra: out }
}

/// Largest image kept temporarily while its chip is shown (a 4K DIB is about 33 MB).
const HOLD_MAX: usize = 40 * 1024 * 1024;

/// Copy of the DIB on the clipboard, so Copy and Save keep working even after the clipboard changed.
/// Only held while the image chip is shown.
pub fn hold_image(owner: HWND) -> Option<Vec<u8>> {
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let dib = with_bytes(CF_DIB, |b| (b.len() <= HOLD_MAX && Dib::parse(b).is_some()).then(|| b.to_vec()));
        let _ = CloseClipboard();
        dib
    }
}

/// Puts the DIB back on the clipboard.
pub fn copy_image(owner: HWND, dib: &[u8]) -> bool {
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let ok = (|| -> Result<()> {
            EmptyClipboard()?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, dib.len())?;
            let ptr = GlobalLock(mem) as *mut u8;
            if ptr.is_null() {
                return Err(Error::from_thread());
            }
            std::ptr::copy_nonoverlapping(dib.as_ptr(), ptr, dib.len());
            let _ = GlobalUnlock(mem);
            SetClipboardData(CF_DIB, Some(HANDLE(mem.0)))?;
            Ok(())
        })()
        .is_ok();
        let _ = CloseClipboard();
        ok
    }
}

/// Saves the image as PNG in Pictures\Screenshots: from `held` if present, otherwise from the
/// clipboard. Returns its path.
pub fn save_image(owner: HWND, held: Option<&[u8]>, stamp: &str) -> Option<PathBuf> {
    let (w, h, pixels) = match held {
        Some(b) => dib_to_bgra(b)?,
        None => unsafe {
            OpenClipboard(Some(owner)).ok()?;
            let dib = with_bytes(CF_DIB, dib_to_bgra);
            let _ = CloseClipboard();
            dib?
        },
    };
    let dir = launcher::pictures_dir()?.join("Screenshots");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("Notch {stamp}.png"));
    write_png(&path, w, h, &pixels).ok()?;
    Some(path)
}

fn write_png(path: &std::path::Path, w: u32, h: u32, bgra: &[u8]) -> Result<()> {
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(Error::empty)?;
        frame.Initialize(None)?;
        frame.SetSize(w, h)?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format)?;
        frame.WritePixels(h, w * 4, bgra)?;
        frame.Commit()?;
        encoder.Commit()
    }
}

/// Waits for a file to finish downloading in Downloads: the browser writes to a temporary file (.crdownload,
/// .part) then renames it. Only a rename from a temporary file counts, so
/// renaming a regular file doesn't produce a chip.
fn watch_downloads(dir: PathBuf) {
    let wide = HSTRING::from(dir.as_os_str());
    let Ok(handle) = (unsafe {
        CreateFileW(
            &wide,
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }) else {
        return;
    };
    // FILE_NOTIFY_INFORMATION must be DWORD-aligned.
    let mut buf = vec![0u32; 4096];
    let mut old: Option<String> = None;
    loop {
        let mut bytes = 0u32;
        let ok = unsafe {
            ReadDirectoryChangesW(handle, buf.as_mut_ptr() as *mut _, (buf.len() * 4) as u32, false, FILE_NOTIFY_CHANGE_FILE_NAME, Some(&mut bytes), None, None)
        };
        if ok.is_err() {
            break;
        }
        let raw = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, bytes as usize) };
        let mut at = 0usize;
        while at + 12 <= raw.len() {
            let next = u32::from_le_bytes(raw[at..at + 4].try_into().unwrap_or_default()) as usize;
            let action = u32::from_le_bytes(raw[at + 4..at + 8].try_into().unwrap_or_default());
            let len = u32::from_le_bytes(raw[at + 8..at + 12].try_into().unwrap_or_default()) as usize;
            let name: Vec<u16> = raw.get(at + 12..at + 12 + len).unwrap_or_default().as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            let name = String::from_utf16_lossy(&name);
            match FILE_ACTION(action) {
                FILE_ACTION_RENAMED_OLD_NAME => old = Some(name),
                FILE_ACTION_RENAMED_NEW_NAME => {
                    let was_partial = old.take().is_some_and(|o| PARTIAL.iter().any(|p| o.to_lowercase().ends_with(p)));
                    let is_partial = PARTIAL.iter().any(|p| name.to_lowercase().ends_with(p));
                    if was_partial && !is_partial && !name.starts_with('~') {
                        let path = Box::new(dir.join(&name).display().to_string());
                        watch::post(WM_APP_DOWNLOAD, 0, Box::into_raw(path) as isize);
                    }
                }
                _ => {}
            }
            if next == 0 {
                break;
            }
            at += next;
        }
    }
    unsafe {
        let _ = CloseHandle(handle);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn malformed_dib_is_rejected() {
        // Negative header (huge after conversion) and an impossible color count.
        let mut b = vec![0u8; 64];
        b[0..4].copy_from_slice(&(-1i32).to_le_bytes());
        b[4..8].copy_from_slice(&4i32.to_le_bytes());
        b[8..12].copy_from_slice(&4i32.to_le_bytes());
        b[14..16].copy_from_slice(&32u16.to_le_bytes());
        assert!(Dib::parse(&b).is_none());
        b[0..4].copy_from_slice(&40i32.to_le_bytes());
        b[32..36].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(Dib::parse(&b).is_none());
        // Data too short for the image size.
        b[32..36].copy_from_slice(&0i32.to_le_bytes());
        assert!(Dib::parse(&b).is_none());
    }

    use super::*;

    /// 2 × 2 24-bit DIB, rows from the bottom (each row padded to a multiple of 4 bytes).
    fn dib() -> Vec<u8> {
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&40i32.to_le_bytes());
        b[4..8].copy_from_slice(&2i32.to_le_bytes());
        b[8..12].copy_from_slice(&2i32.to_le_bytes());
        b[14..16].copy_from_slice(&24u16.to_le_bytes());
        // Bottom row: blue, green. Top row: red, white.
        b.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]);
        b.extend_from_slice(&[0, 0, 255, 255, 255, 255, 0, 0]);
        b
    }

    #[test]
    fn dib_rows_are_flipped_to_top_down() {
        let (w, h, px) = dib_to_bgra(&dib()).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(&px[0..4], &[0, 0, 255, 255], "kiri atas merah");
        assert_eq!(&px[8..12], &[255, 0, 0, 255], "kiri bawah biru");
        assert_eq!(dib_to_bgra(&dib()[..44]), None, "data terpotong ditolak");
    }

    /// Solid-color 32-bit DIB.
    fn flat(w: i32, h: i32, v: u8) -> Vec<u8> {
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&40i32.to_le_bytes());
        b[4..8].copy_from_slice(&w.to_le_bytes());
        b[8..12].copy_from_slice(&h.to_le_bytes());
        b[14..16].copy_from_slice(&32u16.to_le_bytes());
        b.extend(std::iter::repeat_n(v, (w * h.abs() * 4) as usize));
        b
    }

    #[test]
    fn png_is_written() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let path = std::env::temp_dir().join(format!("notch-png-{}.png", std::process::id()));
        write_png(&path, 2, 2, &[200u8; 16]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn thumbnails_keep_aspect_ratio() {
        let big = flat(1920, 1080, 128);
        let t = thumbnail(&big, &Dib::parse(&big).unwrap());
        assert_eq!((t.w, t.h), (96, 54));
        assert_eq!(&t.bgra[0..4], &[128, 128, 128, 255]);
        let small = flat(10, -10, 200);
        let t = thumbnail(&small, &Dib::parse(&small).unwrap());
        assert_eq!((t.w, t.h), (10, 10));
    }
}

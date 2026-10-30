//! "Start with Windows" via HKCU\Software\Microsoft\Windows\CurrentVersion\Run.

use windows::{
    core::*,
    Win32::Foundation::{E_FAIL, ERROR_FILE_NOT_FOUND},
    Win32::System::Registry::*,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("Notch");

pub fn is_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set(enabled: bool) -> Result<()> {
    unsafe {
        if !enabled {
            let err = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE);
            // Already gone also counts as success.
            return if err == ERROR_FILE_NOT_FOUND { Ok(()) } else { err.ok() };
        }
        let exe = std::env::current_exe().map_err(|_| Error::from(E_FAIL))?;
        let command: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(Some(0)).collect();
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            REG_SZ.0,
            Some(command.as_ptr().cast()),
            (command.len() * 2) as u32,
        )
        .ok()
    }
}

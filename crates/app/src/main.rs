//! Notch: a small control center at the top edge of the Windows screen.
//!
//! Arguments:
//!   --toggle              open/close the panel on the running instance
//!   --add "<sentence>"    add tasks from a sentence (split automatically), e.g. "review PR, then meeting at 2"
//!   --glance "<text>"     show a glance (used for testing; later by notch-bridge)
//!   --command "<text>"    open the command bar with that text, e.g. "25m review PR"
//!   --focus               start / pause / resume the pomodoro (same as Ctrl+Alt+P)
//!   --focus-stop          stop the pomodoro
//!   --deck                open the Deck tab
//!   --autostart on|off    set "Start with Windows" then exit
//!   --retry <n>           used by notch itself when relaunching after a failed start

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adhan;
mod agent_setup;
mod agents;
mod app;
mod autostart;
mod brands;
mod chime;
mod chips;
mod clock;
mod codex_quota;
mod command;
mod deck;
mod display;
mod dragdrop;
mod geometry;
mod input;
mod launcher;
mod location;
mod media;
mod island;
mod paint;
mod parse;
mod pet;
mod pipe;
mod pomodoro;
mod prayer;
mod quota;
mod settings;
mod spring;
mod state;
mod svgpath;
mod tasks;
mod theme;
mod tray;
mod watch;

use windows::{
    core::*,
    System::DispatcherQueueController,
    Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
    Win32::System::LibraryLoader::GetModuleHandleW,
    Win32::System::Threading::CreateMutexW,
    Win32::System::WinRT::*,
    Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
    Win32::UI::WindowsAndMessaging::*,
};

fn arg_value(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

/// Right after login (especially with Fast Startup), the graphics driver, DWM or Explorer is sometimes not
/// ready yet, and creating the window or Composition fails. Without a console, notch then exits without a trace.
/// Here the error is logged to `%APPDATA%\Notch\startup.log`, then notch is relaunched as a
/// new process after a pause (no half-built state left over), at most `MAX_RETRY` times.
const MAX_RETRY: u32 = 6;

/// One line to `%APPDATA%\Notch\startup.log`: when notch was launched, started, failed,
/// panicked or exited. Enough to tell what happened at login without a console or debugger.
fn startup_log(line: &str) {
    use std::io::Write;
    let Some(dir) = std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("Notch")) else { return };
    let path = dir.join("startup.log");
    // The last few events are enough; the file must not keep growing.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 16 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let _ = writeln!(f, "{secs} pid {}: {line}", std::process::id());
    }
}

unsafe fn retry_later(mutex: windows::Win32::Foundation::HANDLE, attempt: u32, err: &str) {
    startup_log(&format!("attempt {attempt}: start failed: {err}"));
    if attempt >= MAX_RETRY {
        return;
    }
    // Release the single-instance lock first; otherwise the new process thinks notch is already running.
    let _ = windows::Win32::System::Threading::ReleaseMutex(mutex);
    let _ = windows::Win32::Foundation::CloseHandle(mutex);
    std::thread::sleep(std::time::Duration::from_secs(5 + 5 * attempt as u64));
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--retry").arg((attempt + 1).to_string()).spawn();
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(v) = arg_value(&args, "--autostart") {
        return autostart::set(v == "on");
    }
    let cmd = if let Some(text) = arg_value(&args, "--glance") {
        format!("glance:{text}")
    } else if let Some(text) = arg_value(&args, "--add") {
        format!("add:{text}")
    } else if let Some(text) = arg_value(&args, "--command") {
        format!("command:{text}")
    } else if let Some(px) = arg_value(&args, "--settings-scroll") {
        format!("settings-scroll:{px}")
    } else if let Some(name) = arg_value(&args, "--hover-setting") {
        format!("hover-setting:{name}")
    } else if let Some(name) = arg_value(&args, "--click-setting") {
        format!("click-setting:{name}")
    } else if args.iter().any(|a| a == "--media-flip") {
        "media-flip".to_string()
    } else if args.iter().any(|a| a == "--blink") {
        "blink".to_string()
    } else if args.iter().any(|a| a == "--adhan-stop") {
        "adhan-stop".to_string()
    } else if let Some(w) = arg_value(&args, "--prayer-test") {
        format!("prayer-test:{w}")
    } else if let Some(w) = arg_value(&args, "--focus-test") {
        format!("focus-test:{w}")
    } else if let Some(text) = arg_value(&args, "--clip-test") {
        format!("clip-test:{text}")
    } else if let Some(title) = arg_value(&args, "--media-test") {
        format!("media-test:{title}")
    } else if args.iter().any(|a| a == "--toggle") {
        "toggle".to_string()
    } else if args.iter().any(|a| a == "--focus") {
        "focus".to_string()
    } else if args.iter().any(|a| a == "--focus-stop") {
        "focus-stop".to_string()
    } else if let Some(n) = arg_value(&args, "--today-day") {
        format!("today-day:{n}")
    } else if let Some(t) = arg_value(&args, "--tab") {
        format!("tab:{t}")
    } else if args.iter().any(|a| a == "--agent") {
        "agent".to_string()
    } else if args.iter().any(|a| a == "--deck") {
        "deck".to_string()
    } else if let Some(n) = arg_value(&args, "--deck-edit") {
        format!("deck-edit:{n}")
    } else if let Some(m) = arg_value(&args, "--move-test") {
        format!("move:{m}")
    } else if let Some(c) = arg_value(&args, "--card") {
        format!("card:{c}")
    } else if let Some(n) = arg_value(&args, "--deck-run") {
        format!("deck-run:{n}")
    } else {
        String::new()
    };

    // Which attempt this is (see `retry_later`); 0 for a normal launch.
    let attempt: u32 = arg_value(&args, "--retry").and_then(|n| n.parse().ok()).unwrap_or(0);

    unsafe {
        let mutex = CreateMutexW(None, true, w!("Local\\Notch.SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if cmd.is_empty() {
                startup_log("launched, but notch is already running: toggling it");
            }
            // Launching notch a second time = open/close the panel on the existing instance.
            app::forward(if cmd.is_empty() { "toggle" } else { &cmd });
            return Ok(());
        }

        // A panic on any thread is logged (message and location); without this a release build just disappears.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            startup_log(&format!("panic: {info}"));
            default_hook(info);
        }));
        startup_log(&format!("launched (attempt {attempt})"));

        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<DispatcherQueueController> {
            // Windows.UI.Composition needs a DispatcherQueue on the UI thread.
            let queue = CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?;
            // Debug builds only: NOTCH_FAIL_START=n fails the first n attempts (NOTCH_PANIC_START=n:
            // with a panic), to test the retry.
            let fail_first = |var: &str| cfg!(debug_assertions) && std::env::var(var).ok().and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| attempt < n);
            if fail_first("NOTCH_FAIL_START") {
                return Err(Error::new(windows::Win32::Foundation::E_FAIL, "simulated start failure"));
            }
            if fail_first("NOTCH_PANIC_START") {
                panic!("simulated start panic");
            }
            app::start(GetModuleHandleW(None)?.into())?;
            Ok(queue)
        }));
        let _queue = match started {
            Ok(Ok(q)) => q,
            Ok(Err(e)) => {
                retry_later(mutex, attempt, &e.to_string());
                return Err(e);
            }
            Err(_) => {
                // The panic message was already logged by the hook above.
                retry_later(mutex, attempt, "panic");
                return Err(Error::new(windows::Win32::Foundation::E_FAIL, "panic during start"));
            }
        };
        startup_log("running");
        if !cmd.is_empty() {
            app::command(&cmd);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if app::pre_translate(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        app::shutdown();
        startup_log("exited");
        Ok(())
    }
}

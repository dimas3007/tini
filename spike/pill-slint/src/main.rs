//! Spike: notch pill with Slint (winit + femtovg or software renderer).
//!
//! Left click: expand/collapse. Right click: quit.
//! `--cycle <ms>`: automatic toggle (to measure CPU during animation).

#![windows_subsystem = "windows"]

use std::time::Duration;

use windows::{core::w, Win32::UI::WindowsAndMessaging::*};

slint::slint! {
    export component Pill inherits Window {
        title: "Notch Pill (Slint)";
        no-frame: true;
        always-on-top: true;
        background: transparent;
        width: 680px;
        height: 236px;
        default-font-family: "Segoe UI Variable Text";
        in-out property <bool> expanded: false;
        callback quit();

        Rectangle {
            // Size, radius, and curve are identical to the Composition variant.
            width: root.expanded ? 680px : 190px;
            height: root.expanded ? 236px : 34px;
            x: (root.width - self.width) / 2;
            y: 0px;
            background: #000;
            border-bottom-left-radius: root.expanded ? 28px : 17px;
            border-bottom-right-radius: self.border-bottom-left-radius;
            clip: true;
            animate width, height, border-bottom-left-radius {
                duration: 480ms;
                easing: cubic-bezier(0.32, 1.25, 0.5, 1);
            }

            Rectangle {
                opacity: root.expanded ? 0 : 1;
                animate opacity { delay: 120ms; duration: 220ms; }
                Rectangle { x: 14px; y: 13px; width: 8px; height: 8px; border-radius: 4px; background: #6E8BFF; }
                Text {
                    x: parent.width - self.width - 14px;
                    y: (34px - self.height) / 2;
                    text: "09:41";
                    color: #F2F3F5;
                    font-size: 13px;
                    font-weight: 600;
                }
            }

            Rectangle {
                opacity: root.expanded ? 1 : 0;
                animate opacity { delay: 120ms; duration: 220ms; }
                Text { x: 14px; y: 9px; text: "Rabu, 22 Okt"; color: #8C919C; font-size: 12.5px; }
                VerticalLayout {
                    x: 14px; y: 40px; width: 220px; height: 130px; spacing: 6px; alignment: start;
                    Text { text: "HARI INI · 5"; color: #8C919C; font-size: 10.5px; font-weight: 600; }
                    Text { text: "Standup tim"; color: #F2F3F5; font-size: 12.5px; }
                    Text { text: "Review PR #214 auth refactor"; color: #F2F3F5; font-size: 12.5px; }
                    Text { text: "Meeting sprint 14:00"; color: #F2F3F5; font-size: 12.5px; }
                    Text { text: "Balas email klien invoice"; color: #F2F3F5; font-size: 12.5px; }
                    Text { text: "Update dokumentasi API"; color: #F2F3F5; font-size: 12.5px; }
                }
                VerticalLayout {
                    x: 250px; y: 40px; width: 220px; height: 170px; spacing: 6px; alignment: start;
                    Text { text: "AGENT · CLAUDE CODE"; color: #8C919C; font-size: 10.5px; font-weight: 600; }
                    Text { text: "Read auth.service.ts"; color: #F2F3F5; font-size: 12.5px; }
                    Text { text: "Edit refreshSession()"; color: #F2F3F5; font-size: 12.5px; }
                    Text { text: "Bash · npm test"; color: #F2F3F5; font-size: 12.5px; }
                    Text { text: "Sesi 5 jam · 42%"; color: #8C919C; font-size: 12.5px; }
                    Rectangle { height: 5px; background: #1E2128; Rectangle { x: 0; width: parent.width * 0.42; background: #6E8BFF; } }
                    Text { text: "Mingguan · 18%"; color: #8C919C; font-size: 12.5px; }
                    Rectangle { height: 5px; background: #1E2128; Rectangle { x: 0; width: parent.width * 0.18; background: #46D490; } }
                }
            }

            TouchArea {
                clicked => { root.expanded = !root.expanded; }
                pointer-event(e) => {
                    if (e.button == PointerEventButton.right && e.kind == PointerEventKind.up) {
                        root.quit();
                    }
                }
            }
        }
    }
}

fn cycle_arg() -> Option<u64> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == "--cycle")?;
    args.get(i + 1)?.parse().ok()
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = Pill::new()?;
    ui.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    ui.show()?;

    // The winit window is only really created after the event loop runs, so set position & style afterwards.
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_millis(100), move || {
        let Some(ui) = weak.upgrade() else { return };
        unsafe {
            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let w = (680.0 * ui.window().scale_factor()).round() as i32;
            ui.window().set_position(slint::PhysicalPosition::new((screen_w - w) / 2, 0));
            if let Ok(hwnd) = FindWindowW(None, w!("Notch Pill (Slint)")) {
                // Hide from the taskbar & Alt+Tab, and don't steal focus.
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                let _ = ShowWindow(hwnd, SW_HIDE);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) as isize);
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    });

    let cycle = slint::Timer::default();
    if let Some(ms) = cycle_arg() {
        let weak = ui.as_weak();
        cycle.start(slint::TimerMode::Repeated, Duration::from_millis(ms), move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_expanded(!ui.get_expanded());
            }
        });
    }

    slint::run_event_loop()
}

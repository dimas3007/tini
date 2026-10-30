//! Visual tokens: monochrome palette (black, white, silver) with a single amber accent for urgent things.

use windows::{core::*, Win32::Graphics::Direct2D::Common::D2D1_COLOR_F};

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

/// Apple white (#F5F5F7); secondary and tertiary levels use the same white at lower opacity.
pub const LABEL: D2D1_COLOR_F = rgba(0.961, 0.961, 0.969, 1.0);
pub const SECONDARY: D2D1_COLOR_F = rgba(0.961, 0.961, 0.969, 0.5);
pub const TERTIARY: D2D1_COLOR_F = rgba(0.961, 0.961, 0.969, 0.32);
/// Divider lines and ring track.
pub const HAIRLINE: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.08);
/// Selected tab pill (#1D1D1F).
pub const TAB_FILL: D2D1_COLOR_F = rgba(0.114, 0.114, 0.122, 1.0);
/// Input fields and command box (#161618).
pub const FIELD: D2D1_COLOR_F = rgba(0.086, 0.086, 0.094, 1.0);
/// Graphite for secondary buttons and icon backgrounds (#2C2C2E).
pub const GRAPHITE: D2D1_COLOR_F = rgba(0.173, 0.173, 0.180, 1.0);
pub const BLACK: D2D1_COLOR_F = rgba(0.0, 0.0, 0.0, 1.0);
/// The only color: permission, overdue, quota almost used up (#FFB340).
pub const AMBER: D2D1_COLOR_F = rgba(1.0, 0.702, 0.251, 1.0);
/// Thin outline so the black notch stays readable on a dark background.
pub const HAIRLINE_ALPHA: u8 = 22;

/// Chrome sheen color stops, read conically (conic) starting at 12 o'clock.
pub const CHROME: [f32; 7] = [1.0, 0.557, 0.961, 0.227, 0.820, 0.388, 1.0];

/// Equivalent of SF Pro Text / SF Pro Display on Windows 11.
pub const TEXT_FONT: PCWSTR = w!("Segoe UI Variable Text");
pub const DISPLAY_FONT: PCWSTR = w!("Segoe UI Variable Display");
/// For commands requested by the agent; Windows 11 built-in (automatic fallback if missing).
pub const MONO_FONT: PCWSTR = w!("Cascadia Mono");

/// Chrome brightness at position `t` (0..1) around the circle.
pub fn chrome_at(t: f32) -> f32 {
    let n = CHROME.len() - 1;
    let x = t.rem_euclid(1.0) * n as f32;
    let i = (x as usize).min(n - 1);
    let f = x - i as f32;
    CHROME[i] + (CHROME[i + 1] - CHROME[i]) * f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_wraps_smoothly() {
        assert_eq!(chrome_at(0.0), 1.0);
        assert!((chrome_at(1.0) - chrome_at(0.0)).abs() < 1e-6);
        assert!(chrome_at(0.5) < 0.6);
    }
}

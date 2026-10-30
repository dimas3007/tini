//! Settings stored in %APPDATA%\Notch\settings.ini (key=value format).

use std::path::PathBuf;

use crate::command::{Entry, Kind};
use crate::pomodoro::{Durations, Length};

/// Shortcuts pinned in the command bar, at most this many.
pub const MAX_PINS: usize = 5;
/// Agent project folders remembered for the command bar.
pub const MAX_PROJECTS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorMode {
    /// Stays on the chosen monitor (`Settings::display`); moves only when the notch is dragged to another monitor.
    Stay,
    /// Follows the monitor the active window is on.
    Follow,
}

/// Notch size when idle. Hover always opens one level larger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotchSize {
    /// Only Nochi peeks out; hover becomes a pill.
    Shy,
    /// Pill: Nochi and remaining tasks; hover shows the next task.
    #[default]
    Compact,
    /// Tampilan hover (task berikutnya, aktivitas agent) selalu tampil.
    Full,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub monitor: MonitorMode,
    /// Device name of the chosen monitor for `Stay`, e.g. `\\.\DISPLAY2`; empty = primary monitor.
    pub display: String,
    /// Shrinks to a thin line when the active window occupies the notch area (shy mode).
    pub shy: bool,
    /// Hidden when a fullscreen app or presentation mode is active.
    pub hide_fullscreen: bool,
    /// Focus length, break length, and number of sessions before a long break.
    pub pomodoro: Durations,
    /// Smart chips: greeting, copied links and images, finished downloads.
    pub chips: bool,
    /// Shortcuts pinned in the command bar.
    pub pins: Vec<Entry>,
    /// Last agent project folders, newest first.
    pub projects: Vec<String>,
    /// Date of the last greeting (ISO), so the morning greeting shows only once a day.
    pub greeted: String,
    /// Notch size when idle.
    pub size: NotchSize,
    /// Prayer schedule location; `None` until the user picks a city.
    pub prayer: Option<crate::prayer::Place>,
    /// Prayer schedule widget enabled (with its reminders).
    pub prayer_widget: bool,
    /// Glance 10 minutes before and at prayer time.
    pub prayer_remind: bool,
    /// Soft chime for prayer reminders and the focus timer.
    pub sound: bool,
    /// At prayer time: full adhan (`adhan.mp3`) instead of the normal chime.
    pub adhan: bool,
    /// Copied-text history (memory only).
    pub clip_history: bool,
    /// Claude's 5-hour and weekly quota in the Agent tab. Reads (and refreshes) the Claude Code login,
    /// so it stays off until the user turns it on.
    pub claude_usage: bool,
    /// Codex quota in the Agent tab, read from Codex's own session logs.
    pub codex_usage: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            monitor: MonitorMode::Stay,
            display: String::new(),
            shy: true,
            hide_fullscreen: true,
            pomodoro: Durations::default(),
            chips: true,
            pins: Vec::new(),
            projects: Vec::new(),
            size: NotchSize::Compact,
            prayer: None,
            prayer_widget: true,
            prayer_remind: true,
            sound: true,
            adhan: false,
            clip_history: true,
            claude_usage: false,
            codex_usage: false,
            greeted: String::new(),
        }
    }
}

const POMODORO_KEYS: [(&str, Length); 4] =
    [("focus_min", Length::Focus), ("short_break_min", Length::ShortBreak), ("long_break_min", Length::LongBreak), ("long_break_every", Length::Every)];

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Notch").join("settings.ini"))
}

fn on_off(v: bool) -> &'static str {
    if v {
        "on"
    } else {
        "off"
    }
}

impl Settings {
    pub fn load() -> Self {
        path().and_then(|p| std::fs::read_to_string(p).ok()).map(|s| Self::parse(&s)).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = path() else { return Ok(()) };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(p, self.serialize())
    }

    /// Unknown keys or invalid values are ignored; the rest use defaults.
    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match (k.trim(), v.trim()) {
                // The old "monitor" key (auto/primary) is intentionally ignored: it used to default to following the
                // active window, and a notch that moves monitors on its own is exactly what is not wanted.
                ("display_mode", "stay") => s.monitor = MonitorMode::Stay,
                ("display_mode", "follow") => s.monitor = MonitorMode::Follow,
                ("display", v) => s.display = v.to_string(),
                ("shy", v @ ("on" | "off")) => s.shy = v == "on",
                ("hide_fullscreen", v @ ("on" | "off")) => s.hide_fullscreen = v == "on",
                ("chips", v @ ("on" | "off")) => s.chips = v == "on",
                ("greeted", v) => s.greeted = v.to_string(),
                ("prayer_place", v) => {
                    // "lat,lon,name"
                    let mut parts = v.splitn(3, ',');
                    if let (Some(Ok(lat)), Some(Ok(lon)), Some(name)) = (parts.next().map(str::parse), parts.next().map(str::parse), parts.next()) {
                        s.prayer = Some(crate::prayer::Place { name: name.to_string(), lat, lon });
                    }
                }
                ("notch_size", "shy") => s.size = NotchSize::Shy,
                ("notch_size", "compact") => s.size = NotchSize::Compact,
                ("notch_size", "full") => s.size = NotchSize::Full,
                ("prayer_widget", v @ ("on" | "off")) => s.prayer_widget = v == "on",
                ("prayer_remind", v @ ("on" | "off")) => s.prayer_remind = v == "on",
                // "prayer_chime" is the old key from before the chime was also used by the focus timer.
                ("sound" | "prayer_chime", v @ ("on" | "off")) => s.sound = v == "on",
                ("prayer_sound", v @ ("chime" | "adhan")) => s.adhan = v == "adhan",
                ("clip_history", v @ ("on" | "off")) => s.clip_history = v == "on",
                ("claude_usage", v @ ("on" | "off")) => s.claude_usage = v == "on",
                ("codex_usage", v @ ("on" | "off")) => s.codex_usage = v == "on",
                ("project", v) if !v.is_empty() && s.projects.len() < MAX_PROJECTS => s.projects.push(v.to_string()),
                ("pin", v) if s.pins.len() < MAX_PINS => {
                    let mut parts = v.splitn(3, '|');
                    if let (Some(kind), Some(name), Some(target)) = (parts.next().and_then(Kind::from_code), parts.next(), parts.next()) {
                        s.pins.push(Entry { kind, name: name.to_string(), target: target.to_string() });
                    }
                }
                (k, v) => {
                    if let (Some((_, l)), Ok(n)) = (POMODORO_KEYS.iter().find(|(key, _)| *key == k), v.parse()) {
                        s.pomodoro.set(*l, n);
                    }
                }
            }
        }
        s
    }

    fn serialize(&self) -> String {
        let mode = match self.monitor {
            MonitorMode::Stay => "stay",
            MonitorMode::Follow => "follow",
        };
        let mut out = format!("display_mode={mode}\nshy={}\nhide_fullscreen={}\n", on_off(self.shy), on_off(self.hide_fullscreen));
        if !self.display.is_empty() {
            out.push_str(&format!("display={}\n", self.display));
        }
        for (key, l) in POMODORO_KEYS {
            out.push_str(&format!("{key}={}\n", self.pomodoro.get(l)));
        }
        out.push_str(&format!("chips={}\n", on_off(self.chips)));
        let size = match self.size {
            NotchSize::Shy => "shy",
            NotchSize::Compact => "compact",
            NotchSize::Full => "full",
        };
        out.push_str(&format!("notch_size={size}\n"));
        if let Some(p) = &self.prayer {
            out.push_str(&format!("prayer_place={:.4},{:.4},{}\n", p.lat, p.lon, p.name.replace('\n', " ")));
        }
        out.push_str(&format!(
            "prayer_widget={}\nprayer_remind={}\nsound={}\nprayer_sound={}\nclip_history={}\nclaude_usage={}\ncodex_usage={}\n",
            on_off(self.prayer_widget),
            on_off(self.prayer_remind),
            on_off(self.sound),
            if self.adhan { "adhan" } else { "chime" },
            on_off(self.clip_history),
            on_off(self.claude_usage),
            on_off(self.codex_usage)
        ));
        if !self.greeted.is_empty() {
            out.push_str(&format!("greeted={}\n", self.greeted));
        }
        for p in &self.pins {
            out.push_str(&format!("pin={}|{}|{}\n", p.kind.code(), p.name.replace('|', " "), p.target));
        }
        for p in &self.projects {
            out.push_str(&format!("project={p}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let pomodoro = Durations { focus: 50, short_break: 10, long_break: 30, every: 3 };
        let pins = vec![Entry { kind: Kind::App, name: "Visual Studio Code".into(), target: r"C:\Start\Code.lnk".into() }];
        let projects = vec![r"C:\code\notch".to_string()];
        let prayer = Some(crate::prayer::Place { name: "Near Bandung, ID".into(), lat: -6.9175, lon: 107.6191 });
        let s = Settings {
            monitor: MonitorMode::Follow,
            display: r"\\.\DISPLAY2".into(),
            shy: false,
            hide_fullscreen: false,
            pomodoro,
            chips: false,
            pins,
            projects,
            greeted: "2026-10-23".into(),
            size: NotchSize::Full,
            prayer,
            prayer_widget: false,
            prayer_remind: false,
            sound: false,
            adhan: true,
            clip_history: false,
            claude_usage: true,
            codex_usage: true,
        };
        assert_eq!(Settings::parse(&s.serialize()), s);
        assert_eq!(Settings::parse("garbage\nmonitor = auto\nshy = maybe"), Settings::default());
        // Old files that used to follow the active window now stay on a single monitor.
        assert_eq!(Settings::parse("monitor=auto\n").monitor, MonitorMode::Stay);
        // Out-of-range numbers are clamped; non-numbers are ignored.
        let p = Settings::parse("focus_min=500\nshort_break_min=abc\nlong_break_every=1").pomodoro;
        assert_eq!((p.focus, p.short_break, p.every), (90, 5, 2));
    }
}

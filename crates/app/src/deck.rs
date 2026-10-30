//! Deck: self-configured buttons, like a Stream Deck. Each button opens something, presses a
//! key combination in the active app, runs a command, types text, or
//! runs a notch action. Stored in `%APPDATA%\Notch\deck.json`. Pure (no Win32), except for
//! reading and writing the file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Line icons for buttons that do not use an app icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Glyph {
    App,
    Code,
    Terminal,
    Play,
    MicOff,
    Snip,
    Text,
    Sparkle,
    Timer,
    Folder,
    Globe,
    Keyboard,
    Lock,
}

impl Glyph {
    pub const ALL: [Glyph; 13] = [
        Glyph::App,
        Glyph::Code,
        Glyph::Terminal,
        Glyph::Play,
        Glyph::MicOff,
        Glyph::Snip,
        Glyph::Text,
        Glyph::Sparkle,
        Glyph::Timer,
        Glyph::Folder,
        Glyph::Globe,
        Glyph::Keyboard,
        Glyph::Lock,
    ];

    /// Next icon, for picking an icon by repeated clicks.
    pub fn next(self) -> Glyph {
        let i = Self::ALL.iter().position(|g| *g == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// App, file, folder, or link.
    Open,
    /// Key combination sent to the active app, e.g. "Ctrl+Shift+M".
    Keys,
    /// Command in Windows Terminal.
    Command,
    /// Text typed into the active app.
    Text,
    /// The notch's own action.
    Notch,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Open, Kind::Keys, Kind::Command, Kind::Text, Kind::Notch];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Open => "Open",
            Kind::Keys => "Keys",
            Kind::Command => "Command",
            Kind::Text => "Text",
            Kind::Notch => "Notch",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotchKind {
    /// Focus session; `value` = minutes.
    #[default]
    Focus,
    /// Send a prompt to a new Claude Code session; `value` = prompt, `dir` = repo (empty = last).
    Claude,
    /// Lock the screen.
    Lock,
}

impl NotchKind {
    pub const ALL: [NotchKind; 3] = [NotchKind::Focus, NotchKind::Claude, NotchKind::Lock];

    pub fn label(self) -> &'static str {
        match self {
            NotchKind::Focus => "Focus",
            NotchKind::Claude => "Ask Claude",
            NotchKind::Lock => "Lock screen",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Button {
    pub label: String,
    pub glyph: Glyph,
    pub kind: Kind,
    /// Open: path or link. Keys: "Ctrl+Shift+M". Command: command. Text: text (`\n` = new
    /// line). Notch: minutes (Focus) or prompt (Claude).
    #[serde(default)]
    pub value: String,
    /// Command: working folder. Claude: repo.
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub notch: NotchKind,
    /// The button has an on/off state (e.g. mic muted): marked amber, and shown in the pill while on.
    #[serde(default)]
    pub toggle: bool,
    /// Current state; not saved, so always off after the notch starts.
    #[serde(skip)]
    pub on: bool,
}

impl Button {
    pub fn new(kind: Kind) -> Button {
        let glyph = match kind {
            Kind::Open => Glyph::App,
            Kind::Keys => Glyph::Keyboard,
            Kind::Command => Glyph::Terminal,
            Kind::Text => Glyph::Text,
            Kind::Notch => Glyph::Timer,
        };
        Button { label: String::new(), glyph, kind, value: String::new(), dir: String::new(), notch: NotchKind::Focus, toggle: false, on: false }
    }

    /// This button is ready to run (its required fields are filled in and make sense).
    pub fn problem(&self) -> Option<&'static str> {
        let empty = self.value.trim().is_empty();
        match self.kind {
            Kind::Open if empty => Some("Add a path or link to open"),
            Kind::Keys if parse_keys(&self.value).is_none() => Some("Keys look like Ctrl+Shift+M"),
            Kind::Command if empty => Some("Add a command to run"),
            Kind::Text if empty => Some("Add the text to type"),
            Kind::Notch if self.notch == NotchKind::Focus && self.value.trim().parse::<u32>().map_or(true, |m| m == 0 || m > 240) => {
                Some("Focus needs minutes, 1 to 240")
            }
            Kind::Notch if self.notch == NotchKind::Claude && empty => Some("Add a prompt for Claude"),
            _ => None,
        }
    }

    /// Displayed label: the one entered, or a guess from its content.
    pub fn title(&self) -> String {
        if !self.label.trim().is_empty() {
            return self.label.trim().to_string();
        }
        match self.kind {
            Kind::Open => {
                let v = self.value.trim().trim_end_matches(['/', '\\']);
                let name = v.rsplit(['/', '\\']).next().unwrap_or(v);
                name.rsplit_once('.').filter(|(stem, ext)| !stem.is_empty() && ext.len() <= 4).map_or(name, |(stem, _)| stem).to_string()
            }
            Kind::Keys => self.value.trim().to_string(),
            Kind::Command => self.value.split_whitespace().take(2).collect::<Vec<_>>().join(" "),
            Kind::Text => self.value.chars().take(12).collect(),
            Kind::Notch => match self.notch {
                NotchKind::Focus => format!("Focus {}", self.value.trim()),
                NotchKind::Claude => "Ask Claude".into(),
                NotchKind::Lock => "Lock".into(),
            },
        }
    }
}

/// Windows virtual-key codes used by the deck (part of winuser.h).
pub mod vk {
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const LWIN: u16 = 0x5B;

    pub fn is_modifier(v: u16) -> bool {
        matches!(v, SHIFT | CONTROL | MENU | LWIN | 0x5C | 0xA0..=0xA5)
    }
}

const NAMED: [(&str, u16); 25] = [
    ("Enter", 0x0D),
    ("Esc", 0x1B),
    ("Tab", 0x09),
    ("Space", 0x20),
    ("Backspace", 0x08),
    ("Delete", 0x2E),
    ("Insert", 0x2D),
    ("Up", 0x26),
    ("Down", 0x28),
    ("Left", 0x25),
    ("Right", 0x27),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("PrintScreen", 0x2C),
    ("Mute", 0xAD),
    ("VolumeDown", 0xAE),
    ("VolumeUp", 0xAF),
    ("Next", 0xB0),
    ("Prev", 0xB1),
    ("PlayPause", 0xB3),
    ("Minus", 0xBD),
    ("Plus", 0xBB),
    ("Comma", 0xBC),
];

fn key_name(v: u16) -> String {
    match v {
        vk::CONTROL => "Ctrl".into(),
        vk::SHIFT => "Shift".into(),
        vk::MENU => "Alt".into(),
        vk::LWIN => "Win".into(),
        0x30..=0x39 | 0x41..=0x5A => (v as u8 as char).to_string(),
        0x70..=0x87 => format!("F{}", v - 0x6F),
        _ => NAMED.iter().find(|(_, k)| *k == v).map_or_else(|| format!("0x{v:02X}"), |(n, _)| n.to_string()),
    }
}

/// "Ctrl+Shift+M" → [CONTROL, SHIFT, 'M']: modifiers first, then exactly one main key.
pub fn parse_keys(s: &str) -> Option<Vec<u16>> {
    let mut mods = Vec::new();
    let mut main = None;
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let lower = part.to_lowercase();
        let v = match lower.as_str() {
            "ctrl" | "control" => vk::CONTROL,
            "shift" => vk::SHIFT,
            "alt" => vk::MENU,
            "win" | "windows" | "meta" => vk::LWIN,
            _ if part.len() == 1 && part.chars().all(|c| c.is_ascii_alphanumeric()) => part.to_ascii_uppercase().as_bytes()[0] as u16,
            _ if lower.starts_with('f') && lower[1..].parse::<u16>().is_ok_and(|n| (1..=24).contains(&n)) => 0x6F + lower[1..].parse::<u16>().ok()?,
            _ => NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(part)).map(|(_, k)| *k)?,
        };
        if vk::is_modifier(v) {
            if !mods.contains(&v) {
                mods.push(v);
            }
        } else if main.replace(v).is_some() {
            return None;
        }
    }
    // Modifier order stays as Windows writes it (Win, Ctrl, Alt, Shift), whatever order they were typed in.
    mods.sort_by_key(|m| [vk::LWIN, vk::CONTROL, vk::MENU, vk::SHIFT].iter().position(|x| x == m));
    mods.push(main?);
    Some(mods)
}

pub fn format_keys(keys: &[u16]) -> String {
    keys.iter().map(|k| key_name(*k)).collect::<Vec<_>>().join("+")
}

/// Combination from recorded keys: the modifiers currently held and one main key.
pub fn recorded(ctrl: bool, shift: bool, alt: bool, win: bool, main: u16) -> String {
    let mut keys: Vec<u16> = [(win, vk::LWIN), (ctrl, vk::CONTROL), (alt, vk::MENU), (shift, vk::SHIFT)].iter().filter(|(on, _)| *on).map(|(_, v)| *v).collect();
    keys.push(main);
    format_keys(&keys)
}

/// Text to type: `\n` becomes a new line.
pub fn text_to_type(value: &str) -> String {
    value.replace("\\n", "\n")
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    buttons: Vec<Button>,
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Notch").join("deck.json"))
}

#[derive(Default)]
pub struct Deck {
    pub buttons: Vec<Button>,
}

impl Deck {
    /// `None` if the file does not exist yet (first run); a corrupt file is treated as empty and set aside.
    pub fn load() -> Option<Deck> {
        let p = path()?;
        let text = std::fs::read_to_string(&p).ok()?;
        match serde_json::from_str::<File>(text.trim_start_matches('\u{feff}')) {
            Ok(f) => Some(Deck { buttons: f.buttons }),
            Err(_) => {
                let _ = std::fs::rename(&p, p.with_extension("corrupt.json"));
                Some(Deck::default())
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = path() else { return Ok(()) };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&File { buttons: self.buttons.clone() }).map_err(std::io::Error::other)?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, p)
    }

    /// Initial content: shortcuts that used to be pinned, then a few useful examples.
    pub fn starter(pins: &[(String, String)], downloads: Option<String>) -> Deck {
        let mut buttons: Vec<Button> = pins
            .iter()
            .map(|(name, target)| Button { label: name.clone(), value: target.clone(), ..Button::new(Kind::Open) })
            .collect();
        if let Some(d) = downloads.filter(|d| !buttons.iter().any(|b| &b.value == d)) {
            buttons.push(Button { label: "Downloads".into(), glyph: Glyph::Folder, value: d, ..Button::new(Kind::Open) });
        }
        buttons.push(Button { label: "Terminal".into(), glyph: Glyph::Terminal, value: "wt.exe".into(), ..Button::new(Kind::Open) });
        buttons.push(Button { label: "Snip".into(), glyph: Glyph::Snip, value: "Win+Shift+S".into(), ..Button::new(Kind::Keys) });
        buttons.push(Button { label: "Focus 50".into(), glyph: Glyph::Timer, value: "50".into(), ..Button::new(Kind::Notch) });
        buttons.push(Button { label: "Lock".into(), glyph: Glyph::Lock, notch: NotchKind::Lock, ..Button::new(Kind::Notch) });
        Deck { buttons }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        assert_eq!(parse_keys("Ctrl+Shift+M"), Some(vec![vk::CONTROL, vk::SHIFT, b'M' as u16]));
        assert_eq!(parse_keys("shift + ctrl + m"), Some(vec![vk::CONTROL, vk::SHIFT, b'M' as u16]));
        assert_eq!(format_keys(&parse_keys("shift+win+s").unwrap()), "Win+Shift+S");
        assert_eq!(format_keys(&parse_keys("alt+ctrl+delete").unwrap()), "Ctrl+Alt+Delete");
        assert_eq!(format_keys(&parse_keys("Alt+F4").unwrap()), "Alt+F4");
        assert_eq!(format_keys(&parse_keys("Mute").unwrap()), "Mute");
        assert_eq!(parse_keys("Ctrl+Shift"), None, "tanpa tombol utama");
        assert_eq!(parse_keys("Ctrl+A+B"), None, "dua tombol utama");
        assert_eq!(parse_keys("Ctrl+Banana"), None);
        assert_eq!(recorded(true, true, false, false, b'M' as u16), "Ctrl+Shift+M");
    }

    #[test]
    fn titles_and_problems() {
        let open = Button { value: r"C:\Users\me\Downloads".into(), ..Button::new(Kind::Open) };
        assert_eq!(open.title(), "Downloads");
        let app = Button { value: r"C:\ProgramData\Start Menu\Visual Studio Code.lnk".into(), ..Button::new(Kind::Open) };
        assert_eq!(app.title(), "Visual Studio Code");
        assert_eq!(Button::new(Kind::Keys).problem(), Some("Keys look like Ctrl+Shift+M"));
        let focus = Button { value: "50".into(), ..Button::new(Kind::Notch) };
        assert_eq!((focus.problem(), focus.title().as_str()), (None, "Focus 50"));
        let lock = Button { notch: NotchKind::Lock, ..Button::new(Kind::Notch) };
        assert_eq!(lock.problem(), None);
        assert_eq!(text_to_type(r"Hi,\nJane"), "Hi,\nJane");
    }

    #[test]
    fn starter_keeps_old_pins_and_saves() {
        let d = Deck::starter(&[("VS Code".into(), r"C:\Start\Code.lnk".into())], Some(r"C:\Users\me\Downloads".into()));
        assert_eq!(d.buttons[0].label, "VS Code");
        assert_eq!(d.buttons[1].glyph, Glyph::Folder);
        assert!(d.buttons.iter().all(|b| b.problem().is_none()));
        let json = serde_json::to_string(&File { buttons: d.buttons.clone() }).unwrap();
        assert!(!json.contains("\"on\""), "on/off state is not saved");
        let back: File = serde_json::from_str(&json).unwrap();
        assert_eq!(back.buttons, d.buttons);
    }
}

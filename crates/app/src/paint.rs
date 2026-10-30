//! Draws the notch contents (text, icons, task list, agent sessions) with Direct2D/DirectWrite, in
//! logical units. Besides drawing, the painter returns hit areas, the input field position, and the
//! positions of Nochi and the chrome rings (both separate Composition visuals, driven by DWM).
//!
//! Style: premium monochrome and compact. The notch is 28 px tall when idle; details appear on hover,
//! and the panel (440 × max 300) scrolls its content instead of growing. Translucent white #F5F5F7 over
//! black, silver chrome sheen, SF Symbols-style line icons, and amber reserved for urgent things.

use windows::{
    core::*,
    Win32::Graphics::Direct2D::Common::*,
    Win32::Graphics::Direct2D::*,
    Win32::Graphics::DirectWrite::*,
    Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows_numerics::{Matrix3x2, Vector2};

use crate::agent_setup::McpApp;
use crate::agents::{steps_word, AgentState, Usage, THINKING};
use crate::chips::Thumb;
use crate::deck::{self, Glyph};
use crate::geometry::Shape;
use crate::media::{self, NowPlaying};
use crate::pet;
use crate::pomodoro::{Durations, Length};
use crate::settings::NotchSize;
use crate::svgpath::{self, Seg};
use crate::{brands, theme};

/// Contents for each state. Built once per redraw, so the size of the panel variants doesn't matter.
#[allow(clippy::large_enum_variant)]
pub enum Content {
    Blank,
    /// Only Nochi peeking out.
    Peek,
    /// `music`: a song is playing (small equalizer bars on the right).
    /// `flag`: the deck button that is currently on (amber icon, and its index for turning it off).
    Pill { done: usize, total: usize, music: bool, flag: Option<(Icon, usize)> },
    /// Hovered pill: the next task. `kicker` above the title, e.g. "Next · 10:00 AM".
    Next { kicker: String, warn: bool, title: String, side: Option<String> },
    /// Hovered pill while music plays (or was just paused): artist above the song title, and the
    /// prev / play-pause / next buttons.
    NowPlaying { title: String, artist: String, controls: MediaControls },
    /// One row: bold title then dim detail, e.g. "Claude finished" "12 steps".
    Glance { title: String, detail: Option<String> },
    /// Agent working. One activity row; on `hover`, two rows with the project and time spent working.
    /// `music`: a song is playing, equalizer moves at the far right.
    /// `controls`: when hovered and a song exists, the right column holds the song (`song`: title, artist) above
    /// the media buttons, replacing the equalizer.
    Live {
        activity: String,
        project: String,
        others: usize,
        minutes: u64,
        waiting: bool,
        hover: bool,
        music: bool,
        controls: Option<MediaControls>,
        song: Option<(String, String)>,
    },
    /// Frontmost permission request; `queued`: how many wait behind it.
    /// `wait`: initial wait limit (seconds), for the countdown line. On all three cards, `scroll`:
    /// how far content taller than the canvas has been scrolled (logical).
    Permission { project: String, tool: String, action: String, queued: usize, remaining: u64, wait: u64, hover: Option<Hit>, scroll: f32 },
    /// Choice question from the agent. `options`: (label, description, flagged). `step`: (n-th, of).
    /// `multi`: checkboxes and a Done button; otherwise radio buttons, one click answers at once.
    Question {
        project: String,
        header: Option<String>,
        text: String,
        options: Vec<(String, Option<String>, bool)>,
        multi: bool,
        step: (usize, usize),
        remaining: u64,
        wait: u64,
        hover: Option<Hit>,
        scroll: f32,
    },
    /// Plan awaiting approval.
    Plan { project: String, excerpt: String, remaining: u64, wait: u64, hover: Option<Hit>, scroll: f32 },
    /// Running pomodoro: ring + "19 min"; on `hover`, caption and "18:42". `dim` during
    /// a break or when paused.
    Focus { kicker: String, time: String, progress: f32, paused: bool, dim: bool, hover: bool },
    /// Smart chip: short news with a next-step button.
    SmartChip(Box<ChipView>),
    /// Boxed: much larger than the other variants.
    Expanded(Box<Panel>),
}

/// Prev / play-pause / next buttons for the song that is playing (or just played).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaControls {
    pub playing: bool,
    pub can_prev: bool,
    pub can_next: bool,
    pub hover: Option<Hit>,
}

/// Single-line text that is too long: drawn by the island on its own surface and moved by DWM
/// continuously (like a marquee), so there is no per-frame redraw.
#[derive(Clone, PartialEq)]
pub struct Marquee {
    pub area: Area,
    parts: Vec<(String, Font, D2D1_COLOR_F)>,
    /// Width of one copy of the text (logical).
    pub width: f32,
}

/// Gap between the end of the marquee text and the start of the next copy.
pub const MARQUEE_GAP: f32 = 36.0;

/// Draws two copies of the marquee text side by side (logical), for the island surface.
pub fn draw_marquee(ctx: &ID2D1DeviceContext, d2d: &ID2D1Factory1, dwrite: &IDWriteFactory, m: &Marquee) -> Result<()> {
    let p = Painter { ctx, d2d, dwrite };
    let h = m.area.b - m.area.t;
    for copy in 0..2 {
        let mut x = copy as f32 * (m.width + MARQUEE_GAP);
        for (text, font, color) in &m.parts {
            let w = p.width(*font, text);
            p.text(text, *font, area(x, 0.0, x + w + 2.0, h), *color, Align::Leading)?;
            x += w;
        }
    }
    Ok(())
}

/// What shows on the left of the chip.
#[derive(Clone)]
pub enum Lead {
    /// Nochi inside the orb.
    Pet,
    Icon(Icon),
    /// Thumbnail of the copied image.
    Thumb(Thumb),
    /// Rocking crescent moon with an amber glow (prayer reminder); driven by island.
    Moon,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Plain,
    Primary,
    /// Amber, for urgent things or prompts to start (focus, meeting).
    Amber,
}

/// Chip button: with text (optional icon), or icon only.
#[derive(Clone)]
pub struct ChipButton {
    pub label: Option<String>,
    pub icon: Option<Icon>,
    pub style: ButtonStyle,
}

pub struct ChipView {
    pub lead: Lead,
    pub kicker: String,
    /// Amber caption.
    pub warn: bool,
    pub title: String,
    pub buttons: Vec<ChipButton>,
    pub hover: Option<Hit>,
}

/// Large card above the command bar results.
pub enum CmdCard {
    /// `focus`: focus session (amber); otherwise a regular timer.
    Timer { focus: bool, time: String, detail: String },
    Calc { value: String, expr: String },
    Claude { prompt: String, project: String, can_switch: bool },
    /// Task to be added; `chips`: the day and time that were parsed.
    Task { title: String, chips: Vec<String> },
}

/// One result or suggestion row.
pub struct CmdRow {
    /// Section heading above this row, e.g. "Suggested".
    pub section: Option<&'static str>,
    pub icon: Icon,
    /// Letter in the tile for apps, replacing the icon.
    pub letter: Option<char>,
    pub title: String,
    pub side: String,
    /// `Some`: can be pinned (`true` = already pinned).
    pub pinned: Option<bool>,
}

/// One deck button as drawn.
#[derive(Clone)]
pub struct DeckKey {
    pub label: String,
    pub glyph: Icon,
    /// Real app icon (for the Open button), replacing the line icon.
    pub image: Option<Thumb>,
    /// Hotkey number Ctrl+Alt+N (1..10, 10 written as 0).
    pub number: Option<u8>,
    /// On state (on/off button): marked amber.
    pub on: bool,
}

/// A slice of deck buttons starting at index `first`, and whether the Add box is shown too.
pub struct DeckGrid {
    pub keys: Vec<DeckKey>,
    pub first: usize,
    pub add: bool,
}

/// Deck tab: one grid page.
pub struct DeckPage {
    pub grid: DeckGrid,
    pub page: usize,
    pub pages: usize,
}

/// One field in the button editor. `id`: 0 label, 1 main field, 2 folder.
pub struct EditField {
    pub id: u8,
    pub caption: &'static str,
    pub value: String,
    pub placeholder: &'static str,
    /// Being typed (the EDIT window is placed over it).
    pub active: bool,
    /// For Keys: the combination as keycaps.
    pub keycaps: Option<Vec<String>>,
}

pub struct DeckEditor {
    pub title: String,
    pub glyph: Icon,
    pub image: Option<Thumb>,
    pub label: EditField,
    pub kind: deck::Kind,
    pub notch: deck::NotchKind,
    pub fields: Vec<EditField>,
    pub recording: bool,
    pub toggle: bool,
    /// E.g. "Ctrl+Alt+4"; `None` for buttons after the tenth.
    pub hotkey: Option<String>,
    pub can_delete: bool,
    pub error: Option<String>,
}

/// Commands in the deck button editor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EditCmd {
    Back,
    Delete,
    Glyph,
    Kind(deck::Kind),
    Notch(deck::NotchKind),
    Field(u8),
    Record,
    State,
    Save,
}

/// Line icons for deck button icons.
pub fn glyph_icon(g: Glyph) -> Icon {
    match g {
        Glyph::App => Icon::Grid,
        Glyph::Code => Icon::Code,
        Glyph::Terminal => Icon::Terminal,
        Glyph::Play => Icon::PlayCircle,
        Glyph::MicOff => Icon::MicOff,
        Glyph::Snip => Icon::Snip,
        Glyph::Text => Icon::Lines,
        Glyph::Sparkle => Icon::Sparkle,
        Glyph::Timer => Icon::Timer,
        Glyph::Folder => Icon::Folder,
        Glyph::Globe => Icon::Globe,
        Glyph::Keyboard => Icon::Keyboard,
        Glyph::Lock => Icon::Lock,
    }
}

/// Command bar contents.
pub struct CommandView {
    pub empty: bool,
    pub card: Option<CmdCard>,
    pub rows: Vec<CmdRow>,
    /// `Some` when nothing is typed yet: the first ten deck buttons.
    pub deck: Option<DeckGrid>,
    /// Active choice: 0 = the card (if any), then the rows.
    pub selected: usize,
    /// Key hints at the bottom: (key, caption).
    pub hints: Vec<(&'static str, &'static str)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Today,
    Agent,
    /// Pomodoro.
    Focus,
    /// Opened via the slider button at the top right, not a tab pill.
    Settings,
    /// Command bar: opened via Ctrl+Alt+N or the search button, without a tab row.
    Command,
    /// Self-configured buttons (like Stream Deck).
    Deck,
    /// Widgets: prayer times, clipboard history.
    Widgets,
}

/// Controls on the Settings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Shy,
    HideFullscreen,
    /// `true` = follows the active window, `false` = stays on the chosen monitor.
    FollowApp(bool),
    Autostart,
    /// Smart chips (greeting, clipboard, download).
    Chips,
    Claude,
    /// The notch MCP server in other apps (Claude app, Codex, Antigravity, VS Code).
    Mcp(McpApp),
    /// Copy the notch MCP server entry for other apps.
    CopyMcp,
    /// Claude quota in the Agent tab (reads the Claude Code login).
    ClaudeUsage,
    /// Codex quota in the Agent tab (reads Codex's session logs).
    CodexUsage,
    /// − (`false`) / + (`true`) buttons for the focus and break durations.
    Pomodoro(Length, bool),
    /// Prayer-times widget is on.
    PrayerWidget,
    PrayerRemind,
    /// Sounds for prayer reminders and the focus timer.
    Sound,
    /// Notch size when idle.
    Size(NotchSize),
    /// Sound at prayer time: `false` the regular sound, `true` the full adhan.
    Adhan(bool),
    ClipHistory,
}

/// Short title and description of each setting, for the hover tooltip.
fn setting_tip(s: Setting) -> Option<(&'static str, &'static str)> {
    Some(match s {
        Setting::Shy => ("Peek under windows", "When a maximized window covers the top of the screen, the notch shrinks so only Nochi peeks out. Hover it to bring the notch back."),
        Setting::HideFullscreen => ("Hide in fullscreen", "Hides the notch while a video, game or presentation is fullscreen, so it never covers what you're watching."),
        Setting::Chips => ("Smart chips", "Small helpers that pop out of the notch: a morning greeting, actions for links and screenshots you copy, and finished downloads."),
        Setting::Autostart => ("Start with Windows", "Opens the notch automatically when you sign in to Windows."),
        Setting::Sound => ("Sounds", "Soft sounds when a focus session, break or timer ends, and at prayer times. Turn off for silence everywhere."),
        Setting::FollowApp(false) => ("Stay", "The notch stays on the monitor you put it on. Drag it to move it to another screen."),
        Setting::FollowApp(true) => ("Follow app", "The notch moves to whichever monitor has the app you're using."),
        Setting::PrayerWidget => ("Prayer times", "Shows today's five prayer times for your city in the Widgets tab, worked out on this PC without internet."),
        Setting::ClipHistory => ("Clipboard", "Keeps the last 3 texts you copied in the Widgets tab so you can copy them again. Kept in memory only, never saved."),
        Setting::PrayerRemind => ("Prayer reminders", "A glance 10 minutes before each prayer, and another at the time with the prayer sound."),
        Setting::Adhan(false) => ("Chime", "A short, soft chime at prayer time."),
        Setting::Adhan(true) => ("Adhan", "Plays the full adhan (about 2.5 minutes) at prayer time. Click the notch to stop it."),
        Setting::Claude => ("Claude Code", "Connects the notch to Claude Code: live activity, and approvals and questions answered right here. Turning it off removes the hooks."),
        Setting::ClaudeUsage => ("Claude usage", "Shows your 5-hour and weekly Claude limits in the Agent tab. To get them, the notch reads the Claude Code login on this PC, renews it when it expires, and asks Anthropic only."),
        Setting::CodexUsage => ("Codex usage", "Shows your Codex limits in the Agent tab. The notch reads them from Codex's own session logs on this PC, with no login and no network, so they update each time you use Codex."),
        Setting::Mcp(McpApp::ClaudeApp) => ("Claude app", "Lets chats in the Claude desktop app use the notch: notifications, progress, quick questions, approvals and adding tasks. Restart the Claude app after switching."),
        Setting::Mcp(McpApp::Codex) => ("Codex", "Adds the notch to OpenAI Codex as an MCP server (the Codex app, CLI and IDE extension share one config), so it can notify you, show progress and ask quick questions or approvals here. Restart Codex after switching."),
        Setting::Mcp(McpApp::Antigravity) => ("Antigravity", "Adds the notch to Antigravity as an MCP server, so its agent can notify you, show progress and ask quick questions or approvals here. Restart Antigravity after switching."),
        Setting::Mcp(McpApp::VsCode) => ("VS Code", "Adds the notch to VS Code's MCP servers, so Copilot agent mode can notify you, show progress and ask quick questions or approvals here. VS Code picks it up without a restart."),
        Setting::CopyMcp => ("Other MCP apps", "Copies the notch's MCP server entry. Paste it into the MCP settings of any app that supports local (stdio) servers."),
        Setting::Size(NotchSize::Shy) => ("Shy", "Only Nochi's eyes peek out at the top. Hover to get the pill; agents, focus and alerts still show in full."),
        Setting::Size(NotchSize::Compact) => ("Compact", "The pill: Nochi and how many tasks are left. Hover shows the next task, or the song that's playing."),
        Setting::Size(NotchSize::Full) => ("Full", "The two-line view you'd normally get on hover stays on: your next task and its time, or what the agent is doing."),
        Setting::Pomodoro(..) => return None,
    })
}

/// Values shown on the Settings page.
#[derive(Clone, Copy, Debug, Default)]
pub struct SettingsView {
    pub shy: bool,
    pub hide_fullscreen: bool,
    /// Notch follows the active window's monitor (instead of staying on one monitor).
    pub follow_app: bool,
    pub autostart: bool,
    pub chips: bool,
    /// `None`: notch-bridge.exe not found, so it can't be connected from here.
    pub claude: Option<bool>,
    /// Per app in `McpApp::ALL`; `None`: that app or notch-bridge.exe was not found.
    pub mcp: [Option<bool>; 4],
    /// The MCP server entry was just copied.
    pub mcp_copied: bool,
    pub usage: bool,
    pub codex_usage: bool,
    pub prayer_widget: bool,
    pub prayer_remind: bool,
    pub sound: bool,
    pub size: NotchSize,
    pub adhan: bool,
    pub clip_history: bool,
}

/// Prompt in the Agent tab when there is no session yet: connect an agent, show the quota.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSetup {
    /// Agents that are installed but not yet connected (`Setting::Claude` or `Setting::Mcp`), each
    /// with its own button.
    pub connect: Vec<Setting>,
    /// An agent is already connected.
    pub connected: bool,
    /// Claude quota is still off.
    pub usage: bool,
    /// Codex is installed and its quota is still off.
    pub codex_usage: bool,
}

impl AgentSetup {
    pub fn any(&self) -> bool {
        !self.connect.is_empty() || self.usage || self.codex_usage
    }
}

/// Agent name for the Connect button.
fn connect_label(s: Setting) -> &'static str {
    match s {
        Setting::Mcp(app) => app.name(),
        _ => "Claude Code",
    }
}

fn agent_brand(app: McpApp) -> Brand {
    match app {
        McpApp::ClaudeApp => Brand::Claude,
        McpApp::Codex => Brand::OpenAi,
        McpApp::Antigravity => Brand::Antigravity,
        McpApp::VsCode => Brand::VsCode,
    }
}

fn brand_path(brand: Brand) -> &'static str {
    match brand {
        Brand::Claude => brands::CLAUDE,
        Brand::OpenAi => brands::OPENAI,
        Brand::Antigravity => brands::ANTIGRAVITY,
        Brand::VsCode => brands::VSCODE,
    }
}

/// Actions in the Widgets tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetCmd {
    /// Search for a city (opens the command bar with "city ").
    City,
    /// Use the location from Windows.
    Locate,
    /// Stop the adhan that is playing.
    StopAdhan,
    /// Stop the adhan and use the regular sound until tomorrow.
    MuteAdhan,
    /// Copy the n-th text from the history again.
    Clip(usize),
    ClearClips,
}

/// Prayer-times card.
#[derive(Clone, Debug, Default)]
pub struct PrayerView {
    pub place: String,
    /// "04:12" dst., urut `prayer::NAMES`.
    pub times: [String; 5],
    /// Next prayer, and how many have already passed today.
    pub next: usize,
    pub passed: usize,
    /// "in 1h 12m".
    pub until: String,
    /// Fraction of the way from the previous prayer to the next, 0..1.
    pub progress: f32,
    /// Hijri date, e.g. "12 Jumada I 1448".
    pub hijri: String,
    /// Adhan is playing: its position, e.g. "1:12 / 2:38". The card becomes a player.
    pub adhan: Option<String>,
}

/// Kind of copied text, for its icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipKind {
    Command,
    Link,
    Text,
}

/// One clipboard history row.
#[derive(Clone, Debug)]
pub struct ClipRow {
    pub text: String,
    pub kind: ClipKind,
    /// Time since it was copied, e.g. "2m".
    pub age: String,
}

/// Contents of the Widgets tab.
#[derive(Clone, Debug, Default)]
pub struct WidgetsView {
    /// Prayer-times card is on.
    pub prayer_on: bool,
    /// `None`: no city chosen yet.
    pub prayer: Option<PrayerView>,
    /// Looking up the location, or the message why it failed.
    pub locating: bool,
    pub location_error: Option<String>,
    /// `None`: clipboard history is turned off.
    pub clips: Option<Vec<ClipRow>>,
}

/// Actions on the Focus tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusCmd {
    /// Start, pause, or resume.
    Toggle,
    Stop,
    /// Switch the task being worked on (cycles through today's open tasks).
    Task,
}

/// Focus tab contents.
#[derive(Default)]
pub struct FocusView {
    /// E.g. "FOCUS · 2 OF 4", "SHORT BREAK".
    pub kicker: String,
    pub time: String,
    pub progress: f32,
    /// Running or paused.
    pub active: bool,
    pub paused: bool,
    /// Break or paused: dim ring.
    pub dim: bool,
    /// Task being worked on.
    pub task: Option<String>,
    /// There are open tasks that can be chosen.
    pub can_link: bool,
    /// Sessions completed in this cycle, and the number of sessions before a long break.
    pub done: u32,
    pub every: u32,
    /// Ctrl+Alt+P is registered.
    pub hotkey: bool,
    /// Length of each phase, set directly on this tab.
    pub lengths: Durations,
}

pub struct Chip {
    pub text: String,
    pub warn: bool,
}

pub struct Row {
    pub id: u64,
    pub title: String,
    pub done: bool,
    pub chip: Option<Chip>,
}

pub struct Preview {
    pub title: String,
    pub chip: Option<String>,
}

pub struct WeekDay {
    pub letter: &'static str,
    pub day: u16,
    pub today: bool,
    pub past: bool,
    /// Something to show for that day: unfinished tasks due then, or (past days) tasks
    /// completed then.
    pub busy: bool,
    /// The day whose list is showing.
    pub selected: bool,
}

/// The Today tab is showing another day, not today.
pub struct DayView {
    /// Mis. "Tue, Oct 28".
    pub label: String,
    /// Mis. "4 done", "4 done · 1 open", "2 planned".
    pub detail: String,
    /// Past day (history); otherwise a coming day this week.
    pub past: bool,
}

pub struct AgentRow {
    pub project: String,
    /// Snippet of the last prompt; shown when the row is expanded.
    pub prompt: Option<String>,
    /// Latest actions, newest first; shown when the row is expanded.
    pub history: Vec<String>,
    /// Row expanded (clicked) to see details.
    pub open: bool,
    /// Agent seen only via MCP (no hooks): no step count.
    pub mcp: bool,
    pub activity: String,
    pub last: Option<String>,
    pub state: AgentState,
    pub steps: u32,
    /// Minutes since the last update.
    pub minutes_ago: u64,
    /// Minutes since the last prompt was sent.
    pub minutes_working: u64,
}

pub struct Panel {
    pub tab: Tab,
    /// Task rows visible after scrolling.
    pub rows: Vec<Row>,
    pub more_above: usize,
    pub more_below: usize,
    pub done: usize,
    pub total: usize,
    pub overdue: usize,
    pub hover: Option<Hit>,
    /// `Some` when the input field is open, holding a preview of the parsed sentence.
    pub input: Option<Vec<Preview>>,
    /// Short caption after saving, e.g. "1 task saved for tomorrow".
    pub notice: Option<String>,
    pub week: Vec<WeekDay>,
    /// The day being viewed when it is not today.
    pub day: Option<DayView>,
    /// The week strip can be moved to the previous / next week.
    pub week_prev: bool,
    pub week_next: bool,
    pub agents: Vec<AgentRow>,
    pub agent_working: bool,
    /// Quota from statusLine (only present when Claude Code runs in a terminal).
    pub usage: Option<Usage>,
    /// Latest session context tokens, from the transcript via hooks.
    pub context: Option<u64>,
    /// Buttons on the still-empty Agent tab.
    pub agent_setup: AgentSetup,
    /// Model name for the Agent tab header.
    pub model: Option<String>,
    pub settings: SettingsView,
    /// Settings page scroll position (logical).
    pub settings_scroll: f32,
    /// Windows media session; compact row above the Today tab input field.
    pub media: Option<NowPlaying>,
    /// Agent session list scroll position (logical).
    pub agent_scroll: f32,
    pub focus: FocusView,
    /// Command bar contents (Command tab).
    pub command: Option<CommandView>,
    /// Deck tab: the visible page, or the button editor.
    pub deck: Option<DeckPage>,
    pub editor: Option<DeckEditor>,
    pub widgets: WidgetsView,
    /// A widget is on; without one the Widgets tab is not shown.
    pub widgets_tab: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Toggle(u64),
    Delete(u64),
    Add,
    /// Arrow button in the input field.
    Send,
    /// List area, for scrolling.
    List,
    /// Day n (0 = Monday) in the week strip.
    Day(u8),
    /// Arrow at the end of the week strip.
    WeekPrev,
    WeekNext,
    /// "Today" button when viewing another day.
    BackToToday,
    Allow,
    Deny,
    Tab(Tab),
    Setting(Setting),
    Media(media::Command),
    /// Session rows in the Agent tab, in display order.
    Agent(usize),
    /// n-th option on the question card.
    Choice(usize),
    /// Done choosing (multi-select question).
    Done,
    /// Answer in the agent app: the card is closed without a decision.
    InApp,
    Focus(FocusCmd),
    /// n-th choice in the command bar (0 = the card if any).
    Cmd(usize),
    /// Switch project on the Claude card.
    CmdProject,
    /// n-th deck button.
    DeckKey(usize),
    DeckAdd,
    /// n-th on/off button marker on the pill: click to turn it off.
    DeckFlag(usize),
    Edit(EditCmd),
    /// Pin button on the n-th result row: adds or removes a deck button.
    PinToggle(usize),
    /// n-th button on the chip.
    Chip(usize),
    Widget(WidgetCmd),
}

/// One usage meter row in the Agent tab.
pub struct Meter {
    pub label: &'static str,
    /// Used portion, 0..1.
    pub fraction: f32,
    pub value: String,
    /// Opacity of the bar fill (later meters get dimmer).
    pub alpha: f32,
    /// Time left until the quota resets, e.g. "2h 15m".
    pub note: Option<String>,
}

/// Assumed context window: 200k, or 1M if usage has already exceeded it
/// (the transcript doesn't state the window size).
fn context_window(tokens: u64) -> u64 {
    if tokens > 200_000 {
        1_000_000
    } else {
        200_000
    }
}

fn tokens_label(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// Meters that have data: context from the transcript (or statusLine), then the 5-hour and
/// weekly quotas that only statusLine sends. Without data, no row.
pub fn meters(context: Option<u64>, usage: Option<&Usage>) -> Vec<Meter> {
    let mut out = Vec::new();
    match (context, usage.and_then(|u| u.context_pct)) {
        (Some(t), _) => out.push(Meter { label: "Context", fraction: t as f32 / context_window(t) as f32, value: tokens_label(t), alpha: 1.0, note: None }),
        (None, Some(pct)) => out.push(Meter { label: "Context", fraction: pct / 100.0, value: format!("{pct:.0}%"), alpha: 1.0, note: None }),
        (None, None) => {}
    }
    if let Some(u) = usage {
        for (label, pct, resets, alpha) in [("5-hour", u.five_hour_pct, u.five_hour_resets, 0.7), ("Weekly", u.seven_day_pct, u.seven_day_resets, 0.45)] {
            if let Some(p) = pct {
                let note = resets.and_then(crate::quota::until);
                out.push(Meter { label, fraction: p / 100.0, value: format!("{p:.0}%"), alpha, note });
            }
        }
        // Shorter window first, like Claude's 5-hour before Weekly.
        let mut codex = u.codex.clone();
        codex.sort_by_key(|w| w.minutes);
        for (i, w) in codex.iter().enumerate() {
            let label = match w.minutes {
                300 => "Codex 5-hour",
                10080 => "Codex weekly",
                _ => "Codex",
            };
            let note = w.resets_at.and_then(crate::quota::until);
            let alpha = if i == 0 { 0.7 } else { 0.45 };
            out.push(Meter { label, fraction: w.used_pct / 100.0, value: format!("{:.0}%", w.used_pct), alpha, note });
        }
    }
    out
}

/// Logical square in canvas coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Area {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
}

/// Where Nochi is drawn (face center, canvas logical).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PetSpot {
    pub center: (f32, f32),
    pub size: pet::Size,
}

/// Spinning chrome ring: its center and diameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingSpot {
    pub center: (f32, f32),
    pub diameter: f32,
}

/// Moving crescent moon: center and icon size (canvas logical); `glow`: amber glow pulsing
/// behind it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoonSpot {
    pub center: (f32, f32),
    pub size: f32,
    pub glow: bool,
}

#[derive(Default)]
pub struct Painted {
    /// Sorted from most specific; lookup takes the first match.
    pub hits: Vec<(Area, Hit)>,
    pub input: Option<Area>,
    pub pet: Option<PetSpot>,
    /// Spinning chrome rings, one per session that is working and visible.
    pub rings: Vec<RingSpot>,
    /// Center of the moving equalizer (canvas logical), for the song that is playing.
    pub eq: Option<(f32, f32)>,
    /// Song title that is too long, scrolling in place.
    pub marquee: Option<Marquee>,
    /// Rocking crescent moon (prayer reminder, prayer card in Widgets).
    pub moon: Option<MoonSpot>,
}

impl Painted {
    pub fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().find(|(a, _)| a.contains(x, y)).map(|(_, h)| *h)
    }
}

/// Panel layout; also used by `app` for panel height and row count.
pub mod layout {
    use crate::geometry::CANVAS_H;

    pub const M: f32 = 16.0;
    /// Tab row.
    pub const HEAD: f32 = 40.0;
    /// This-week strip and progress ring (Today tab).
    pub const STRIP_H: f32 = 40.0;
    pub const ROW_H: f32 = 30.0;
    pub const MAX_ROWS: usize = 5;
    /// Tallest task list area; the rest scrolls.
    pub const LIST_MAX: f32 = MAX_ROWS as f32 * ROW_H;
    pub const PREVIEW_H: f32 = 22.0;
    pub const MAX_PREVIEW: usize = 3;
    pub const EMPTY_H: f32 = 60.0;
    pub const BAR_GAP: f32 = 6.0;
    pub const BAR_H: f32 = 32.0;
    pub const BOTTOM: f32 = 12.0;
    /// Media row above the input field; its space is taken from the task list area.
    pub const MEDIA_H: f32 = 30.0;
    pub const AGENT_PITCH: f32 = 46.0;
    pub const AGENT_ROW_H: f32 = 42.0;
    /// One detail row (prompt or action) in an expanded session.
    pub const DETAIL_H: f32 = 16.0;
    /// Limit on rings spinning at once; the rest are static rings.
    pub const MAX_RINGS: usize = 4;
    /// Gap of the divider above the usage meters, and the height of each meter.
    /// Divider and the "Usage · model" title row above the meters.
    pub const USAGE_TOP: f32 = 28.0;
    pub const METER_H: f32 = 20.0;
    pub const SECTION_H: f32 = 24.0;
    pub const SECTION_GAP: f32 = 8.0;
    pub const SETTING_ROW_H: f32 = 36.0;
    /// The first Settings row has a caption.
    pub const SETTING_ROW_TALL: f32 = 40.0;
    pub const SETTINGS_VIEW_H: f32 = CANVAS_H - HEAD - BOTTOM;
    pub const SETTINGS_CONTENT_H: f32 = 3.0 * SECTION_H + 2.0 * SECTION_GAP + 4.0 * SETTING_ROW_TALL + 13.0 * SETTING_ROW_H + DECK_KEY_H + 4.0;
    /// Agent tab without sessions: title and hint, then Connect buttons (three per row) and Show usage.
    pub const SETUP_TEXT_H: f32 = 46.0;
    /// Row of agent logos: a 44 px tile with the name below.
    pub const SETUP_LOGO_H: f32 = 72.0;
    pub const SETUP_ROW_H: f32 = 32.0;
    pub const SETUP_COLS: usize = 6;

    pub fn agent_setup_height(s: &super::AgentSetup) -> f32 {
        let logos = s.connect.len().div_ceil(SETUP_COLS) as f32 * SETUP_LOGO_H;
        SETUP_TEXT_H + logos + (s.usage || s.codex_usage) as u8 as f32 * SETUP_ROW_H - 2.0
    }
    /// Row of phase-length controls in the Focus tab (two columns, two rows).
    pub const LENGTH_ROW_H: f32 = 34.0;
    /// Focus tab content below the tab row: big ring and time, session dots, then the phase-length controls.
    pub const FOCUS_BODY: f32 = 110.0 + 10.0 + 2.0 * LENGTH_ROW_H;
    /// Widgets tab: prayer schedule card (or a prompt to pick a city), then clipboard history.
    pub const PRAYER_H: f32 = 124.0;
    pub const PRAYER_EMPTY_H: f32 = 92.0;
    pub const WIDGET_GAP: f32 = 8.0;
    /// Clipboard card header (top gap + title).
    pub const CLIP_HEAD: f32 = 30.0;
    pub const CLIP_ROW_H: f32 = 26.0;

    /// `prayer`: `None` if the card is off, otherwise whether a city is chosen. `clips`: `None`
    /// if clipboard history is off.
    pub fn widgets_height(prayer: Option<bool>, clips: Option<usize>) -> f32 {
        let prayer_h = prayer.map_or(0.0, |set| if set { PRAYER_H } else { PRAYER_EMPTY_H });
        let gap = if prayer.is_some() { WIDGET_GAP } else { 0.0 };
        let clips_h = clips.map_or(0.0, |n| gap + CLIP_HEAD + n.max(1) as f32 * CLIP_ROW_H + 6.0);
        HEAD + 2.0 + prayer_h + clips_h + BOTTOM
    }

    /// One mouse-wheel notch on the Settings page.
    pub const SETTINGS_STEP: f32 = SETTING_ROW_H;

    /// Row above the task list when the input field is open: preview of the parsed sentence, or
    /// a one-line caption after saving. `None` if there is none; merely opening the input field
    /// adds no row, so the panel doesn't jump.
    pub fn preview_lines(previews: usize, notice: bool) -> Option<usize> {
        let n = if previews > 0 { previews.min(MAX_PREVIEW) } else { notice as usize };
        (n > 0).then_some(n)
    }

    /// Tallest list area; the media row uses part of it, so the panel still fits in `CANVAS_H`.
    pub fn list_max(media: bool) -> f32 {
        if media {
            LIST_MAX - MEDIA_H
        } else {
            LIST_MAX
        }
    }

    /// Task rows that fit in the list area, after subtracting the preview row.
    pub fn task_rows_fit(lines: Option<usize>, media: bool) -> usize {
        let used = lines.map_or(0.0, |n| n.min(MAX_PREVIEW) as f32 * PREVIEW_H);
        ((list_max(media) - used) / ROW_H).floor().max(0.0) as usize
    }

    /// `rows`: visible task rows. Without tasks, the list area is always as tall as the Nochi illustration;
    /// the preview is shown beside it.
    pub fn list_height(rows: usize, lines: Option<usize>, media: bool) -> f32 {
        if rows == 0 {
            return EMPTY_H;
        }
        let extra = lines.map_or(0.0, |n| n.min(MAX_PREVIEW) as f32 * PREVIEW_H);
        (rows.min(MAX_ROWS) as f32 * ROW_H + extra).min(list_max(media))
    }

    pub fn today_height(rows: usize, preview: Option<usize>, media: bool) -> f32 {
        let media_h = if media { MEDIA_H } else { 0.0 };
        HEAD + STRIP_H + list_height(rows, preview, media) + media_h + BAR_GAP + BAR_H + BOTTOM
    }

    pub fn usage_height(meters: usize) -> f32 {
        if meters == 0 {
            0.0
        } else {
            USAGE_TOP + meters as f32 * METER_H
        }
    }

    /// Card awaiting an answer: fixed head (Nochi, title, buttons), then content whose height
    /// follows the text and scrolls once the card is as tall as the canvas. See `super::card_height`.
    pub const CARD_M: f32 = 14.0;
    pub const CARD_HEAD: f32 = 48.0;
    /// Width of the card content.
    pub const CARD_BODY_W: f32 = crate::geometry::CARD_W - 2.0 * CARD_M;
    /// One mouse-wheel notch in the card content.
    pub const CARD_STEP: f32 = 40.0;
    /// Single-line option row (label and description side by side); longer ones grow taller.
    pub const OPTION_H: f32 = 30.0;
    pub const OPTION_GAP: f32 = 4.0;
    /// Option text starts after the radio button / checkbox.
    pub const OPTION_TEXT_L: f32 = 32.0;
    pub const OPTION_TEXT_W: f32 = CARD_BODY_W - OPTION_TEXT_L - 12.0;
    /// Top and bottom margin of an option row whose text wraps.
    pub const OPTION_PAD: f32 = 7.5;

    pub fn agent_row_height(row: &super::AgentRow) -> f32 {
        let lines = if row.open { row.prompt.is_some() as usize + row.history.len() } else { 0 };
        AGENT_ROW_H + if lines > 0 { 4.0 + lines as f32 * DETAIL_H } else { 0.0 }
    }

    /// Height of the whole session list (can be taller than what is visible).
    pub fn agent_list_height(rows: &[super::AgentRow]) -> f32 {
        if rows.is_empty() {
            return AGENT_ROW_H;
        }
        rows.iter().map(agent_row_height).sum::<f32>() + (rows.len() - 1) as f32 * (AGENT_PITCH - AGENT_ROW_H)
    }

    /// Height of the visible list area; the rest scrolls.
    pub fn agent_view_height(list: f32, meters: usize) -> f32 {
        list.min(CANVAS_H - HEAD - 2.0 - usage_height(meters) - BOTTOM)
    }

    pub fn agent_height(list: f32, meters: usize) -> f32 {
        HEAD + 2.0 + agent_view_height(list, meters) + usage_height(meters) + BOTTOM
    }

    pub fn focus_height() -> f32 {
        HEAD + FOCUS_BODY + BOTTOM
    }

    pub fn settings_height() -> f32 {
        CANVAS_H
    }

    pub fn settings_max_scroll() -> f32 {
        (SETTINGS_CONTENT_H - SETTINGS_VIEW_H).max(0.0)
    }

    /// Command bar: field on top, then card, result rows, pins, and key hints.
    pub const CMD_FIELD_T: f32 = 8.0;
    pub const CMD_FIELD_H: f32 = 40.0;
    pub const CMD_BODY: f32 = CMD_FIELD_T + CMD_FIELD_H + 6.0;
    pub const CMD_ROW_H: f32 = 36.0;
    pub const CMD_SECTION_H: f32 = 22.0;
    pub const CMD_CARD_GAP: f32 = 6.0;
    pub const CMD_FOOT_H: f32 = 34.0;

    pub fn cmd_card_height(card: &super::CmdCard) -> f32 {
        use super::CmdCard::*;
        match card {
            Timer { .. } => 86.0,
            Calc { .. } => 70.0,
            Claude { .. } => 92.0,
            Task { .. } => 64.0,
        }
    }

    pub fn command_height(v: &super::CommandView) -> f32 {
        let card = v.card.as_ref().map_or(0.0, |c| cmd_card_height(c) + CMD_CARD_GAP);
        let sections = v.rows.iter().filter(|r| r.section.is_some()).count() as f32 * CMD_SECTION_H;
        let deck = v.deck.as_ref().map_or(0.0, |g| CMD_SECTION_H + deck_rows(g, CMD_DECK_ROWS) as f32 * DECK_KEY_H);
        (CMD_BODY + card + v.rows.len() as f32 * CMD_ROW_H + sections + deck + CMD_FOOT_H).min(CANVAS_H)
    }

    /// Deck: five columns; each button is a 42 px tile with its label below.
    pub const DECK_COLS: usize = 5;
    pub const DECK_KEY_H: f32 = 64.0;
    /// Deck rows in the Deck tab and in the command bar.
    pub const DECK_ROWS: usize = 3;
    pub const CMD_DECK_ROWS: usize = 2;
    /// Buttons per page of the Deck tab.
    pub const DECK_PAGE: usize = DECK_COLS * DECK_ROWS;
    /// One field row in the button editor.
    pub const EDIT_ROW_H: f32 = 32.0;

    /// Rows used by the grid (including the Add box), at most `max`.
    pub fn deck_rows(g: &super::DeckGrid, max: usize) -> usize {
        (g.keys.len() + g.add as usize).div_ceil(DECK_COLS).clamp(1, max)
    }

    /// Deck tab height: rows used on this page (all pages are three rows if there is more
    /// than one, so the panel doesn't change height when switching pages).
    pub fn deck_height(rows: usize, pages: usize) -> f32 {
        let rows = if pages > 1 { DECK_ROWS } else { rows.clamp(1, DECK_ROWS) };
        HEAD + 4.0 + rows as f32 * DECK_KEY_H + if pages > 1 { 14.0 } else { 0.0 } + BOTTOM
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Font {
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
}

const fn text(size: f32, weight: DWRITE_FONT_WEIGHT) -> Font {
    Font { family: theme::TEXT_FONT, size, weight }
}

const fn display(size: f32, weight: DWRITE_FONT_WEIGHT) -> Font {
    Font { family: theme::DISPLAY_FONT, size, weight }
}

const REGULAR: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT_NORMAL;
const SEMI: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT_SEMI_BOLD;
const BOLD: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT_BOLD;

/// Tab names on the segmented track.
const TAB_SEG: Font = text(11.5, SEMI);
const SMALL: Font = text(11.5, REGULAR);
const TINY: Font = text(11.0, REGULAR);
const TINY_STRONG: Font = text(11.0, SEMI);
const ROW: Font = text(13.0, REGULAR);
const ROW_STRONG: Font = text(13.0, SEMI);
const CHIP: Font = display(11.5, REGULAR);
const PREVIEW: Font = text(12.0, REGULAR);
const FIELD: Font = text(12.5, REGULAR);
const WEEKDAY: Font = text(9.5, SEMI);
const DAY: Font = display(12.5, REGULAR);
const DAY_TODAY: Font = display(11.5, BOLD);
const SUMMARY: Font = text(12.5, SEMI);
const EMPTY_TITLE: Font = display(13.5, SEMI);
const AGENT_LINE: Font = text(12.0, REGULAR);
const SECTION: Font = text(11.0, SEMI);
const PILL_COUNT: Font = text(12.0, SEMI);
const LIVE_NOW: Font = text(12.5, SEMI);
const NEXT_TITLE: Font = text(14.0, SEMI);
const GLANCE_TITLE: Font = text(13.0, SEMI);
const GLANCE_DETAIL: Font = text(12.0, REGULAR);
const PERM_TITLE: Font = text(13.5, SEMI);
const BANG: Font = display(13.0, DWRITE_FONT_WEIGHT_EXTRA_BOLD);
const FOCUS_TIME: Font = display(30.0, SEMI);
const FOCUS_HOVER_TIME: Font = display(16.0, SEMI);
/// Widget card: name of the prayer whose adhan is playing.
const WIDGET_BIG: Font = display(21.0, BOLD);
/// Prayer card: "Dhuhr 11:37" beside the ring, the time left, then the name and time in the pill.
const PRAYER_HERO: Font = display(26.0, BOLD);
const PRAYER_UNTIL: Font = text(12.5, SEMI);
const PRAYER_PILL_NAME: Font = text(10.5, REGULAR);
const PRAYER_PILL_TIME: Font = text(12.0, SEMI);
const MONO: Font = Font { family: theme::MONO_FONT, size: 12.0, weight: REGULAR };
/// Command on the clipboard card: slightly smaller so it doesn't look bolder than other text.
const MONO_SMALL: Font = Font { family: theme::MONO_FONT, size: 11.0, weight: REGULAR };
const CMD_BIG: Font = display(26.0, SEMI);
const BADGE: Font = text(9.0, SEMI);
const DECK_LABEL: Font = text(10.5, REGULAR);

/// Space outside the text in the glance shape: Nochi on the left, gap, and right margin.
pub const GLANCE_CHROME_W: f32 = 44.0 + 16.0;
/// Gap between the glance title and caption.
const GLANCE_GAP: f32 = 8.0;

/// Largest diameter of the spinning chrome ring; its sprite is scaled for other sizes.
pub const RING_MAX: f32 = 36.0;

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Leading,
    Center,
    Trailing,
}

/// Agent logo, white on a graphite tile.
#[derive(Clone, Copy)]
enum Brand {
    Claude,
    OpenAi,
    Antigravity,
    VsCode,
}

/// SF Symbols-style line icons, drawn in 24×24 space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Icon {
    Check,
    Today,
    Sparkle,
    Plus,
    ArrowUp,
    Cross,
    /// Three sliders, for Settings.
    Sliders,
    Minus,
    /// Stopwatch, for the Focus tab.
    Timer,
    Play,
    Pause,
    Stop,
    /// Previous / next song: triangle with a vertical bar.
    Backward,
    Forward,
    /// Four static equalizer bars, music marker.
    Waveform,
    Folder,
    /// "< >", for VS Code.
    Code,
    Globe,
    Download,
    /// Pin tack.
    Pin,
    Copy,
    Search,
    /// Four squares: Deck tab and generic app icon.
    Grid,
    /// Crescent moon (prayer times).
    Moon,
    /// Widgets tab: one wide tile and two small tiles (dashboard).
    Widgets,
    /// Deck tab: a 3×2 button pad, so it doesn't look like the Widgets icon.
    Keypad,
    /// Clipboard (clipboard history card).
    Clipboard,
    /// Chain (text that is a link).
    Link,
    Terminal,
    PlayCircle,
    MicOff,
    Snip,
    /// Text rows.
    Lines,
    Keyboard,
    Lock,
    Trash,
    ChevronLeft,
    ChevronRight,
}

/// First letter of the name for an app tile.
pub fn initial(name: &str) -> char {
    name.chars().find(|c| c.is_alphanumeric()).map_or('?', |c| c.to_ascii_uppercase())
}

fn area(l: f32, t: f32, r: f32, b: f32) -> Area {
    Area { l, t, r, b }
}

fn rect(a: Area) -> D2D_RECT_F {
    D2D_RECT_F { left: a.l, top: a.t, right: a.r, bottom: a.b }
}

fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn with_alpha(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a, ..c }
}

fn gray(v: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: v, g: v, b: v, a }
}

fn white(a: f32) -> D2D1_COLOR_F {
    with_alpha(theme::LABEL, a)
}

/// Glance title and caption from one text: the part before the first " · " becomes the title.
pub fn glance_parts(text: &str) -> (String, Option<String>) {
    match text.split_once(" · ") {
        Some((title, detail)) => (title.to_string(), Some(detail.to_string())),
        None => (text.to_string(), None),
    }
}

fn minutes_ago(m: u64) -> String {
    match m {
        0 => "just now".into(),
        1..=59 => format!("{m} min ago"),
        _ => format!("{} h ago", m / 60),
    }
}

fn duration(m: u64) -> String {
    match m {
        0 => "<1 min".into(),
        1..=59 => format!("{m} min"),
        _ => format!("{} h {} min", m / 60, m % 60),
    }
}

fn format(dwrite: &IDWriteFactory, font: Font, align: Align, wrap: bool) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = dwrite.CreateTextFormat(font.family, None, font.weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, font.size, w!("en-us"))?;
        f.SetTextAlignment(match align {
            Align::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::Trailing => DWRITE_TEXT_ALIGNMENT_TRAILING,
        })?;
        // One row is centered vertically; text that may wrap is aligned to the top.
        f.SetParagraphAlignment(if wrap { DWRITE_PARAGRAPH_ALIGNMENT_NEAR } else { DWRITE_PARAGRAPH_ALIGNMENT_CENTER })?;
        f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
        if !wrap {
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            let ellipsis = dwrite.CreateEllipsisTrimmingSign(&f)?;
            f.SetTrimming(&trim, &ellipsis)?;
        }
        Ok(f)
    }
}

fn text_width(dwrite: &IDWriteFactory, font: Font, text: &str) -> f32 {
    let measure = || -> Result<f32> {
        let f = format(dwrite, font, Align::Leading, false)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let layout = dwrite.CreateTextLayout(&wide, &f, 10_000.0, 100.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok(m.widthIncludingTrailingWhitespace)
        }
    };
    measure().unwrap_or(text.chars().count() as f32 * font.size * 0.55)
}

/// Width of single-row glance text (title, gap, caption) in logical pixels.
pub fn glance_text_width(dwrite: &IDWriteFactory, text: &str) -> f32 {
    let (title, detail) = glance_parts(text);
    let detail_w = detail.map_or(0.0, |d| GLANCE_GAP + text_width(dwrite, GLANCE_DETAIL, &d));
    text_width(dwrite, GLANCE_TITLE, &title) + detail_w
}

/// Height of text wrapped to width `w` (logical).
fn text_height(dwrite: &IDWriteFactory, font: Font, text: &str, w: f32) -> f32 {
    let measure = || -> Result<f32> {
        let f = format(dwrite, font, Align::Leading, true)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let layout = dwrite.CreateTextLayout(&wide, &f, w, 10_000.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok(m.height)
        }
    };
    measure().unwrap_or(font.size * 1.4)
}

/// Size of the answer-awaiting card content, from measured text. Used for the shape height
/// (`card_height`) and when drawing, so the two always match. Text is never clipped:
/// content taller than the canvas scrolls.
struct CardBody {
    /// Scrollable content, starting right below the head.
    h: f32,
    /// Fixed space below the content: margin, or the plan buttons.
    foot: f32,
    /// Main text height: command, question, or plan.
    text_h: f32,
    /// Question options: row height, and label height when the description wraps below the label.
    rows: Vec<(f32, Option<f32>)>,
}

impl CardBody {
    fn natural(&self) -> f32 {
        layout::CARD_HEAD + self.h + self.foot
    }
}

/// Command box on the permission card; MCP `request_approval` text without the "$" prompt.
fn permission_body(dwrite: &IDWriteFactory, approval: bool, action: &str) -> CardBody {
    let text_w = layout::CARD_BODY_W - if approval { 24.0 } else { 38.0 };
    let text_h = text_height(dwrite, MONO, action, text_w);
    CardBody { h: 2.0 + (text_h + 14.0).max(30.0) + 10.0, foot: 14.0, text_h, rows: Vec::new() }
}

/// Question text, then one row per option (and a Done row for multi-select). Label and
/// description sit side by side if they fit on one line; otherwise the description drops below the label
/// and both wrap.
fn question_body(dwrite: &IDWriteFactory, text: &str, options: &[(String, Option<String>, bool)], multi: bool) -> CardBody {
    use layout::*;
    let text_h = text_height(dwrite, ROW_STRONG, text, CARD_BODY_W);
    let rows: Vec<(f32, Option<f32>)> = options
        .iter()
        .map(|(label, detail, _)| {
            let inline = text_width(dwrite, TINY_STRONG, label) + detail.as_ref().map_or(0.0, |d| 8.0 + text_width(dwrite, TINY, d));
            if inline <= OPTION_TEXT_W {
                return (OPTION_H, None);
            }
            let label_h = text_height(dwrite, TINY_STRONG, label, OPTION_TEXT_W);
            let detail_h = detail.as_ref().map_or(0.0, |d| 1.0 + text_height(dwrite, TINY, d, OPTION_TEXT_W));
            ((2.0 * OPTION_PAD + label_h + detail_h).max(OPTION_H), Some(label_h))
        })
        .collect();
    let options_h: f32 = rows.iter().map(|(h, _)| h + OPTION_GAP).sum::<f32>() + if multi { OPTION_H + OPTION_GAP } else { 0.0 };
    CardBody { h: text_h + 8.0 + options_h, foot: 8.0, text_h, rows }
}

/// Plan with buttons that stay at the bottom.
fn plan_body(dwrite: &IDWriteFactory, excerpt: &str) -> CardBody {
    let text_h = text_height(dwrite, SMALL, excerpt, layout::CARD_BODY_W);
    CardBody { h: 2.0 + text_h + 8.0, foot: 8.0 + layout::OPTION_H + 16.0, text_h, rows: Vec::new() }
}

/// Card height needed so all its text is visible without scrolling; 0 for other content.
/// The shape is capped at `CANVAS_H`, and the difference becomes the scroll limit.
pub fn card_height(dwrite: &IDWriteFactory, content: &Content) -> f32 {
    match content {
        Content::Permission { tool, action, .. } => permission_body(dwrite, tool == "approval", action).natural(),
        Content::Question { text, options, multi, .. } => question_body(dwrite, text, options, *multi).natural(),
        Content::Plan { excerpt, .. } => plan_body(dwrite, excerpt).natural(),
        _ => 0.0,
    }
}

/// Silver ring with a circular glint: short arcs with successive chrome brightness.
pub fn chrome_ring(ctx: &ID2D1DeviceContext, d2d: &ID2D1Factory1, cx: f32, cy: f32, r: f32, width: f32, alpha: f32) -> Result<()> {
    const SEGMENTS: usize = 64;
    unsafe {
        for i in 0..SEGMENTS {
            // Start at 12 o'clock, clockwise; slight overlap so there are no gaps.
            let a0 = std::f32::consts::TAU * i as f32 / SEGMENTS as f32 - std::f32::consts::FRAC_PI_2;
            let a1 = std::f32::consts::TAU * (i as f32 + 1.15) / SEGMENTS as f32 - std::f32::consts::FRAC_PI_2;
            let v = theme::chrome_at((i as f32 + 0.5) / SEGMENTS as f32);
            let brush = ctx.CreateSolidColorBrush(&gray(v, alpha), None)?;
            let path = d2d.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.BeginFigure(pt(cx + r * a0.cos(), cy + r * a0.sin()), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: pt(cx + r * a1.cos(), cy + r * a1.sin()),
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            ctx.DrawGeometry(&path, &brush, width, None);
        }
    }
    Ok(())
}

/// Size of the crescent moon on its surface (logical); island scales it to the `MoonSpot` size.
pub const MOON_ICON: f32 = 20.0;
/// Moon surface box (logical), slightly larger than the icon so the stroke is not clipped.
pub const MOON_BOX: f32 = 24.0;
/// Glow surface box behind the moon (logical), before scaling.
pub const MOON_GLOW: f32 = 30.0;

/// Line crescent moon for the island surface, centered in the `MOON_BOX` box. Drawn once; its motion
/// is run by DWM.
pub fn draw_moon(ctx: &ID2D1DeviceContext, d2d: &ID2D1Factory1, dwrite: &IDWriteFactory) -> Result<()> {
    let c = MOON_BOX / 2.0;
    Painter { ctx, d2d, dwrite }.icon(Icon::Moon, c, c, MOON_ICON, 1.8, theme::LABEL)
}

/// Soft amber glow behind the moon: a radial gradient fading to transparent.
pub fn draw_moon_glow(ctx: &ID2D1DeviceContext, d2d: &ID2D1Factory1, dwrite: &IDWriteFactory) -> Result<()> {
    let p = Painter { ctx, d2d, dwrite };
    let c = MOON_GLOW / 2.0;
    let stops = p.stops(&[(0.0, with_alpha(theme::AMBER, 0.5)), (0.5, with_alpha(theme::AMBER, 0.16)), (1.0, with_alpha(theme::AMBER, 0.0))])?;
    let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: pt(c, c), gradientOriginOffset: pt(0.0, 0.0), radiusX: c, radiusY: c };
    unsafe {
        let brush = ctx.CreateRadialGradientBrush(&props, None, &stops)?;
        ctx.FillEllipse(&D2D1_ELLIPSE { point: pt(c, c), radiusX: c, radiusY: c }, &brush);
    }
    Ok(())
}

pub struct Painter<'a> {
    pub ctx: &'a ID2D1DeviceContext,
    pub d2d: &'a ID2D1Factory1,
    pub dwrite: &'a IDWriteFactory,
}

impl Painter<'_> {
    fn brush(&self, color: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        unsafe { self.ctx.CreateSolidColorBrush(&color, None) }
    }

    fn stops(&self, stops: &[(f32, D2D1_COLOR_F)]) -> Result<ID2D1GradientStopCollection> {
        let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(position, color)| D2D1_GRADIENT_STOP { position, color }).collect();
        // ID2D1RenderTarget version; ID2D1DeviceContext has an overload with the same name.
        let rt: &ID2D1RenderTarget = self.ctx;
        unsafe { rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) }
    }

    fn text(&self, s: &str, font: Font, a: Area, color: D2D1_COLOR_F, align: Align) -> Result<()> {
        self.draw_text(s, font, a, color, align, false)
    }

    fn draw_text(&self, s: &str, font: Font, a: Area, color: D2D1_COLOR_F, align: Align, wrap: bool) -> Result<()> {
        let f = format(self.dwrite, font, align, wrap)?;
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.ctx.DrawText(&wide, &f, &rect(a), &self.brush(color)?, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
        Ok(())
    }

    fn width(&self, font: Font, s: &str) -> f32 {
        text_width(self.dwrite, font, s)
    }

    /// Single-line text with strikethrough, for completed tasks.
    fn struck(&self, s: &str, font: Font, a: Area, color: D2D1_COLOR_F) -> Result<()> {
        let f = format(self.dwrite, font, Align::Leading, false)?;
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let layout = self.dwrite.CreateTextLayout(&wide, &f, a.r - a.l, a.b - a.t)?;
            layout.SetStrikethrough(true, DWRITE_TEXT_RANGE { startPosition: 0, length: wide.len() as u32 })?;
            self.ctx.DrawTextLayout(pt(a.l, a.t), &layout, &self.brush(color)?, D2D1_DRAW_TEXT_OPTIONS_CLIP);
        }
        Ok(())
    }

    fn circle(&self, cx: f32, cy: f32, r: f32, color: D2D1_COLOR_F) -> Result<()> {
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        unsafe { self.ctx.FillEllipse(&e, &self.brush(color)?) };
        Ok(())
    }

    fn outline(&self, cx: f32, cy: f32, r: f32, width: f32, color: D2D1_COLOR_F) -> Result<()> {
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        unsafe { self.ctx.DrawEllipse(&e, &self.brush(color)?, width, None) };
        Ok(())
    }

    /// Circle with a radial gradient from the top, like a dark glass button.
    fn orb(&self, cx: f32, cy: f32, r: f32, top: D2D1_COLOR_F, bottom: D2D1_COLOR_F) -> Result<()> {
        let stops = self.stops(&[(0.0, top), (1.0, bottom)])?;
        let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
            center: pt(cx, cy - r * 0.4),
            gradientOriginOffset: pt(0.0, 0.0),
            radiusX: r * 1.5,
            radiusY: r * 1.5,
        };
        unsafe {
            let brush = self.ctx.CreateRadialGradientBrush(&props, None, &stops)?;
            self.ctx.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r }, &brush);
        }
        Ok(())
    }

    fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, color: D2D1_COLOR_F) -> Result<()> {
        unsafe { self.ctx.DrawLine(pt(x0, y0), pt(x1, y1), &self.brush(color)?, 1.0, None) };
        Ok(())
    }

    fn round_rect(&self, a: Area, radius: f32, color: D2D1_COLOR_F) -> Result<()> {
        let rr = D2D1_ROUNDED_RECT { rect: rect(a), radiusX: radius, radiusY: radius };
        unsafe { self.ctx.FillRoundedRectangle(&rr, &self.brush(color)?) };
        Ok(())
    }

    /// Rounded-rect outline, e.g. an empty checkbox.
    fn round_outline(&self, a: Area, radius: f32, width: f32, color: D2D1_COLOR_F) -> Result<()> {
        let rr = D2D1_ROUNDED_RECT { rect: rect(a), radiusX: radius, radiusY: radius };
        unsafe { self.ctx.DrawRoundedRectangle(&rr, &self.brush(color)?, width, None) };
        Ok(())
    }

    /// Gradient from transparent to black, marking content that can still be scrolled.
    fn fade(&self, a: Area, down: bool) -> Result<()> {
        let stops = self.stops(&[(0.0, gray(0.0, 0.0)), (1.0, gray(0.0, 1.0))])?;
        let (from, to) = if down { (a.t, a.b) } else { (a.b, a.t) };
        unsafe {
            let brush = self.ctx.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(a.l, from), endPoint: pt(a.l, to) },
                None,
                &stops,
            )?;
            self.ctx.FillRectangle(&rect(a), &brush);
        }
        Ok(())
    }

    /// Thin scroll line on the right edge of the `view` area: `start` and `len` as fractions of the content (0..1).
    fn scrollbar(&self, view: Area, start: f32, len: f32) -> Result<()> {
        let h = view.b - view.t - 4.0;
        let top = view.t + 2.0 + h * start.clamp(0.0, 1.0);
        let len = (h * len).clamp(16.0, h);
        self.round_rect(area(view.r - 3.0, top, view.r, (top + len).min(view.b - 2.0)), 1.5, white(0.22))
    }

    /// Graphite squircle with a chrome edge, container for Nochi and the logo.
    fn tile(&self, a: Area, radius: f32) -> Result<()> {
        let rr = D2D1_ROUNDED_RECT { rect: rect(a), radiusX: radius, radiusY: radius };
        let fill = self.stops(&[(0.0, gray(0.165, 1.0)), (1.0, gray(0.055, 1.0))])?;
        let edge = self.stops(&[(0.0, gray(1.0, 0.6)), (0.35, gray(0.45, 0.4)), (0.65, gray(0.9, 0.5)), (1.0, gray(0.35, 0.35))])?;
        unsafe {
            let fill = self.ctx.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(a.l, a.t), endPoint: pt(a.l + (a.r - a.l) * 0.35, a.b) },
                None,
                &fill,
            )?;
            self.ctx.FillRoundedRectangle(&rr, &fill);
            let edge = self.ctx.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(a.l, a.t), endPoint: pt(a.r, a.b) },
                None,
                &edge,
            )?;
            let inner = D2D1_ROUNDED_RECT { rect: rect(area(a.l + 0.5, a.t + 0.5, a.r - 0.5, a.b - 0.5)), radiusX: radius - 0.5, radiusY: radius - 0.5 };
            self.ctx.DrawRoundedRectangle(&inner, &edge, 1.0, None);
        }
        Ok(())
    }

    /// Progress ring starting at 12 o'clock, clockwise.
    fn progress_ring(&self, cx: f32, cy: f32, r: f32, width: f32, progress: f32, color: D2D1_COLOR_F) -> Result<()> {
        self.outline(cx, cy, r, width, white(0.12))?;
        if progress >= 0.999 {
            return self.outline(cx, cy, r, width, color);
        }
        if progress <= 0.0 {
            return Ok(());
        }
        let angle = std::f32::consts::TAU * progress - std::f32::consts::FRAC_PI_2;
        unsafe {
            let path = self.d2d.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.BeginFigure(pt(cx, cy - r), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: pt(cx + r * angle.cos(), cy + r * angle.sin()),
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if progress > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            self.ctx.DrawGeometry(&path, &self.brush(color)?, width, &self.round_stroke()?);
        }
        Ok(())
    }

    fn round_stroke(&self) -> Result<ID2D1StrokeStyle> {
        let props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
        };
        let factory: &ID2D1Factory = self.d2d;
        unsafe { factory.CreateStrokeStyle(&props, None) }
    }

    /// Line icon of size `size` with stroke width `stroke` (logical pixels), centered at (cx, cy).
    fn icon(&self, icon: Icon, cx: f32, cy: f32, size: f32, stroke: f32, color: D2D1_COLOR_F) -> Result<()> {
        let k = size / 24.0;
        let w = stroke / k;
        let brush = self.brush(color)?;
        let style = self.round_stroke()?;
        unsafe {
            let mut base = Matrix3x2::default();
            self.ctx.GetTransform(&mut base);
            self.ctx.SetTransform(&(Matrix3x2::scale(k, k) * Matrix3x2::translation(cx - 12.0 * k, cy - 12.0 * k) * base));
            let poly = |points: &[(f32, f32)], closed: bool| -> Result<()> {
                let path = self.d2d.CreatePathGeometry()?;
                let sink = path.Open()?;
                sink.BeginFigure(pt(points[0].0, points[0].1), D2D1_FIGURE_BEGIN_HOLLOW);
                for p in &points[1..] {
                    sink.AddLine(pt(p.0, p.1));
                }
                sink.EndFigure(if closed { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
                sink.Close()?;
                self.ctx.DrawGeometry(&path, &brush, w, &style);
                Ok(())
            };
            let rounded = |l: f32, t: f32, r: f32, b: f32, radius: f32| {
                let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: l, top: t, right: r, bottom: b }, radiusX: radius, radiusY: radius };
                self.ctx.DrawRoundedRectangle(&rr, &brush, w, &style);
            };
            let ellipse = |x: f32, y: f32, rx: f32, ry: f32| {
                let e = D2D1_ELLIPSE { point: pt(x, y), radiusX: rx, radiusY: ry };
                self.ctx.DrawEllipse(&e, &brush, w, &style);
            };
            let result = match icon {
                Icon::Check => poly(&[(5.0, 12.5), (9.5, 17.0), (19.0, 7.5)], false),
                Icon::Today => {
                    rounded(3.0, 3.0, 21.0, 21.0, 5.0);
                    poly(&[(8.0, 12.5), (11.0, 15.5), (16.0, 9.5)], false)
                }
                Icon::Sparkle => poly(
                    &[(12.0, 3.0), (13.9, 8.6), (19.5, 10.0), (13.9, 11.9), (12.0, 17.5), (10.1, 11.9), (4.5, 10.0), (10.1, 8.6)],
                    true,
                ),
                Icon::Plus => poly(&[(12.0, 5.0), (12.0, 19.0)], false).and_then(|_| poly(&[(5.0, 12.0), (19.0, 12.0)], false)),
                Icon::Minus => poly(&[(5.0, 12.0), (19.0, 12.0)], false),
                Icon::Timer => {
                    let e = D2D1_ELLIPSE { point: pt(12.0, 13.5), radiusX: 7.5, radiusY: 7.5 };
                    self.ctx.DrawEllipse(&e, &brush, w, &style);
                    poly(&[(10.0, 3.0), (14.0, 3.0)], false)
                        .and_then(|_| poly(&[(12.0, 3.0), (12.0, 6.0)], false))
                        .and_then(|_| poly(&[(12.0, 13.5), (12.0, 9.5)], false))
                }
                Icon::Play => poly(&[(8.0, 5.5), (18.5, 12.0), (8.0, 18.5)], true),
                Icon::Pause => poly(&[(9.0, 6.0), (9.0, 18.0)], false).and_then(|_| poly(&[(15.0, 6.0), (15.0, 18.0)], false)),
                Icon::Stop => {
                    rounded(6.5, 6.5, 17.5, 17.5, 2.5);
                    Ok(())
                }
                Icon::ArrowUp => poly(&[(12.0, 19.0), (12.0, 5.0)], false).and_then(|_| poly(&[(6.0, 11.0), (12.0, 5.0), (18.0, 11.0)], false)),
                Icon::Cross => poly(&[(6.0, 6.0), (18.0, 18.0)], false).and_then(|_| poly(&[(18.0, 6.0), (6.0, 18.0)], false)),
                Icon::Sliders => {
                    // Dashed line at the knob, so the knob looks hollowed out.
                    for (y, knob) in [(6.0, 15.0), (12.0, 9.0), (18.0, 15.0)] {
                        let e = D2D1_ELLIPSE { point: pt(knob, y), radiusX: 2.2, radiusY: 2.2 };
                        self.ctx.DrawEllipse(&e, &brush, w, &style);
                    }
                    [(6.0, 4.0, 12.8), (6.0, 17.2, 20.0), (12.0, 4.0, 6.8), (12.0, 11.2, 20.0), (18.0, 4.0, 12.8), (18.0, 17.2, 20.0)]
                        .iter()
                        .try_for_each(|&(y, a, b)| poly(&[(a, y), (b, y)], false))
                }
                Icon::Backward => poly(&[(19.0, 6.0), (9.5, 12.0), (19.0, 18.0)], true).and_then(|_| poly(&[(5.5, 6.0), (5.5, 18.0)], false)),
                Icon::Forward => poly(&[(5.0, 6.0), (14.5, 12.0), (5.0, 18.0)], true).and_then(|_| poly(&[(18.5, 6.0), (18.5, 18.0)], false)),
                Icon::Waveform => [(6.0, 10.0, 14.0), (10.0, 5.0, 19.0), (14.0, 8.0, 16.0), (18.0, 10.5, 13.5)]
                    .iter()
                    .try_for_each(|&(x, a, b)| poly(&[(x, a), (x, b)], false)),
                Icon::Folder => poly(
                    &[(3.0, 8.0), (3.0, 6.5), (4.5, 5.0), (9.0, 5.0), (11.0, 7.5), (19.5, 7.5), (21.0, 9.0), (21.0, 17.5), (19.5, 19.0), (4.5, 19.0), (3.0, 17.5)],
                    true,
                ),
                Icon::Code => poly(&[(8.0, 7.0), (3.0, 12.0), (8.0, 17.0)], false).and_then(|_| poly(&[(16.0, 7.0), (21.0, 12.0), (16.0, 17.0)], false)),
                Icon::Globe => {
                    ellipse(12.0, 12.0, 9.0, 9.0);
                    ellipse(12.0, 12.0, 4.0, 9.0);
                    poly(&[(3.0, 12.0), (21.0, 12.0)], false)
                }
                Icon::Download => poly(&[(12.0, 4.0), (12.0, 15.0)], false)
                    .and_then(|_| poly(&[(7.0, 10.5), (12.0, 15.5), (17.0, 10.5)], false))
                    .and_then(|_| poly(&[(5.0, 20.0), (19.0, 20.0)], false)),
                Icon::Pin => poly(&[(9.0, 4.0), (15.0, 4.0), (14.0, 10.0), (17.0, 13.0), (7.0, 13.0), (10.0, 10.0)], true)
                    .and_then(|_| poly(&[(12.0, 13.0), (12.0, 20.0)], false)),
                Icon::Copy => {
                    rounded(8.0, 8.0, 20.0, 20.0, 3.0);
                    poly(&[(16.0, 8.0), (16.0, 5.5), (14.5, 4.0), (5.5, 4.0), (4.0, 5.5), (4.0, 14.5), (5.5, 16.0), (8.0, 16.0)], false)
                }
                Icon::Search => {
                    ellipse(11.0, 11.0, 6.5, 6.5);
                    poly(&[(16.0, 16.0), (20.0, 20.0)], false)
                }
                Icon::Moon => poly(
                    &[
                        (11.1, 3.6), (8.6, 4.2), (6.4, 5.6), (4.7, 7.6), (3.7, 10.0), (3.5, 12.6), (4.1, 15.1), (5.4, 17.3),
                        (7.3, 19.1), (9.7, 20.2), (12.2, 20.5), (14.8, 20.0), (17.1, 18.8), (18.9, 17.0), (20.1, 14.7), (17.7, 15.7),
                        (15.0, 15.7), (12.6, 14.8), (10.6, 13.1), (9.4, 10.8), (9.0, 8.3), (9.6, 5.7),
                    ],
                    true,
                ),
                Icon::Widgets => {
                    rounded(3.0, 3.5, 21.0, 12.5, 2.5);
                    rounded(3.0, 15.0, 11.0, 20.5, 2.0);
                    rounded(13.0, 15.0, 21.0, 20.5, 2.0);
                    Ok(())
                }
                Icon::Keypad => {
                    for (x, y) in [(2.5, 5.5), (9.25, 5.5), (16.0, 5.5), (2.5, 13.0), (9.25, 13.0), (16.0, 13.0)] {
                        rounded(x, y, x + 5.5, y + 5.5, 1.6);
                    }
                    Ok(())
                }
                Icon::Clipboard => {
                    rounded(8.0, 3.0, 16.0, 7.0, 1.5);
                    poly(&[(8.0, 5.0), (6.5, 5.0), (4.0, 7.5), (4.0, 18.5), (6.5, 21.0), (17.5, 21.0), (20.0, 18.5), (20.0, 7.5), (17.5, 5.0), (16.0, 5.0)], false)
                }
                Icon::Link => poly(&[(10.0, 14.0), (12.0, 15.2), (15.7, 14.0), (18.7, 11.0), (19.0, 7.0), (17.0, 5.0), (13.0, 5.3), (12.0, 6.3)], false)
                    .and_then(|_| poly(&[(14.0, 10.0), (12.0, 8.8), (8.3, 10.0), (5.3, 13.0), (5.0, 17.0), (7.0, 19.0), (11.0, 18.7), (12.0, 17.7)], false)),
                Icon::Grid => {
                    for (x, y) in [(3.5, 3.5), (13.5, 3.5), (3.5, 13.5), (13.5, 13.5)] {
                        rounded(x, y, x + 7.0, y + 7.0, 2.0);
                    }
                    Ok(())
                }
                Icon::Terminal => {
                    rounded(3.0, 4.0, 21.0, 20.0, 3.0);
                    poly(&[(7.0, 9.0), (10.0, 12.0), (7.0, 15.0)], false).and_then(|_| poly(&[(12.5, 15.0), (17.0, 15.0)], false))
                }
                Icon::PlayCircle => {
                    ellipse(12.0, 12.0, 9.0, 9.0);
                    poly(&[(10.0, 8.5), (15.5, 12.0), (10.0, 15.5)], true)
                }
                Icon::MicOff => {
                    rounded(9.0, 3.0, 15.0, 14.0, 3.0);
                    poly(&[(5.0, 11.0), (5.6, 13.6), (8.0, 16.6), (12.0, 18.0), (16.0, 16.6), (18.4, 13.6), (19.0, 11.0)], false)
                        .and_then(|_| poly(&[(12.0, 18.0), (12.0, 21.0)], false))
                        .and_then(|_| poly(&[(3.5, 3.5), (20.5, 20.5)], false))
                }
                Icon::Snip => {
                    ellipse(12.0, 12.0, 2.5, 2.5);
                    [
                        [(4.0, 8.0), (4.0, 4.0), (8.0, 4.0)],
                        [(16.0, 4.0), (20.0, 4.0), (20.0, 8.0)],
                        [(20.0, 16.0), (20.0, 20.0), (16.0, 20.0)],
                        [(8.0, 20.0), (4.0, 20.0), (4.0, 16.0)],
                    ]
                    .iter()
                    .try_for_each(|c| poly(c, false))
                }
                Icon::Lines => poly(&[(5.0, 7.0), (19.0, 7.0)], false)
                    .and_then(|_| poly(&[(5.0, 12.0), (19.0, 12.0)], false))
                    .and_then(|_| poly(&[(5.0, 17.0), (13.0, 17.0)], false)),
                Icon::Keyboard => {
                    rounded(2.5, 6.0, 21.5, 18.0, 3.0);
                    [(6.5, 7.5), (10.5, 11.5), (14.5, 15.5)]
                        .iter()
                        .try_for_each(|&(a, b)| poly(&[(a, 10.0), (b, 10.0)], false))
                        .and_then(|_| poly(&[(7.0, 14.0), (17.0, 14.0)], false))
                }
                Icon::Lock => {
                    rounded(5.0, 11.0, 19.0, 20.0, 2.5);
                    poly(&[(8.0, 11.0), (8.0, 8.0), (9.2, 5.2), (12.0, 4.0), (14.8, 5.2), (16.0, 8.0), (16.0, 11.0)], false)
                }
                Icon::Trash => poly(&[(4.0, 7.0), (20.0, 7.0)], false)
                    .and_then(|_| poly(&[(9.0, 7.0), (9.0, 4.0), (15.0, 4.0), (15.0, 7.0)], false))
                    .and_then(|_| poly(&[(6.0, 7.0), (7.0, 20.0), (17.0, 20.0), (18.0, 7.0)], false)),
                Icon::ChevronLeft => poly(&[(15.0, 6.0), (9.0, 12.0), (15.0, 18.0)], false),
                Icon::ChevronRight => poly(&[(9.0, 6.0), (15.0, 12.0), (9.0, 18.0)], false),

            };
            self.ctx.SetTransform(&base);
            result
        }
    }

    /// SVG path 24×24 (`d`) filled with `color`, of size `size` and centered at (cx, cy).
    fn svg(&self, d: &str, cx: f32, cy: f32, size: f32, color: D2D1_COLOR_F) -> Result<()> {
        let k = size / 24.0;
        unsafe {
            let path = self.d2d.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
            let (mut open, mut start, mut cur) = (false, pt(0.0, 0.0), pt(0.0, 0.0));
            let p = |(x, y): (f32, f32)| pt(x, y);
            for seg in svgpath::parse(d) {
                if let Seg::Move(x, y) = seg {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    }
                    start = pt(x, y);
                    cur = start;
                    sink.BeginFigure(start, D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                    continue;
                }
                if seg == Seg::Close {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                    cur = start;
                    continue;
                }
                if !open {
                    sink.BeginFigure(cur, D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                }
                match seg {
                    Seg::Line(x, y) => {
                        cur = pt(x, y);
                        sink.AddLine(cur);
                    }
                    Seg::Cubic([a, b, e]) => {
                        cur = p(e);
                        sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(a), point2: p(b), point3: cur });
                    }
                    Seg::Quad([a, e]) => {
                        cur = p(e);
                        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: p(a), point2: cur });
                    }
                    Seg::Arc { to, rx, ry, rotation, large, sweep } => {
                        cur = p(to);
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: cur,
                            size: D2D_SIZE_F { width: rx, height: ry },
                            rotationAngle: rotation,
                            // The SVG y axis points down, same as Direct2D: sweep 1 = clockwise.
                            sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                            arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                        });
                    }
                    Seg::Move(..) | Seg::Close => {}
                }
            }
            if open {
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            }
            sink.Close()?;
            let mut base = Matrix3x2::default();
            self.ctx.GetTransform(&mut base);
            self.ctx.SetTransform(&(Matrix3x2::scale(k, k) * Matrix3x2::translation(cx - 12.0 * k, cy - 12.0 * k) * base));
            self.ctx.FillGeometry(&path, &self.brush(color)?, None);
            self.ctx.SetTransform(&base);
        }
        Ok(())
    }

    /// 22×22 graphite tile containing a white brand logo.
    fn brand(&self, brand: Brand, cx: f32, cy: f32, alpha: f32) -> Result<()> {
        self.tile(area(cx - 11.0, cy - 11.0, cx + 11.0, cy + 11.0), 7.0)?;
        let color = white(alpha);
        self.svg(brand_path(brand), cx, cy, 13.0, color)
    }

    /// MCP app logo on a graphite tile; Codex uses the OpenAI logo.
    fn agent_logo(&self, app: McpApp, cx: f32, cy: f32, alpha: f32) -> Result<()> {
        self.brand(agent_brand(app), cx, cy, alpha)
    }

    /// Static Nochi eyes for the session list in the panel, matching the agent status.
    fn eyes(&self, cx: f32, cy: f32, state: AgentState) -> Result<()> {
        match state {
            AgentState::Done => {
                // "^ ^": top arc of the circle.
                for x in [cx - 4.0, cx + 4.0] {
                    unsafe {
                        let path = self.d2d.CreatePathGeometry()?;
                        let sink = path.Open()?;
                        let r = 2.6;
                        sink.BeginFigure(pt(x - r, cy + 1.2), D2D1_FIGURE_BEGIN_HOLLOW);
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: pt(x + r, cy + 1.2),
                            size: D2D_SIZE_F { width: r, height: r },
                            rotationAngle: 0.0,
                            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                            arcSize: D2D1_ARC_SIZE_SMALL,
                        });
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        sink.Close()?;
                        self.ctx.DrawGeometry(&path, &self.brush(theme::LABEL)?, 1.6, &self.round_stroke()?);
                    }
                }
            }
            _ => {
                let (w, h, half, alpha) = match state {
                    AgentState::Working => (3.5, 3.5, 3.2, 1.0),
                    AgentState::Waiting => (6.0, 6.0, 4.5, 1.0),
                    _ => (3.5, 6.0, 3.5, 0.6),
                };
                for x in [cx - half, cx + half] {
                    self.round_rect(area(x - w / 2.0 - 1.2, cy - h / 2.0 - 1.2, x + w / 2.0 + 1.2, cy + h / 2.0 + 1.2), w / 2.0 + 1.2, white(0.16))?;
                    self.round_rect(area(x - w / 2.0, cy - h / 2.0, x + w / 2.0, cy + h / 2.0), w.min(h) / 2.0, white(alpha))?;
                }
            }
        }
        Ok(())
    }

    /// `canvas_w` and all coordinates in logical pixels; the shape is at the center of the canvas.
    pub fn paint(&self, content: &Content, shape: &Shape, canvas_w: f32) -> Result<Painted> {
        let left = (canvas_w - shape.w) / 2.0;
        let right = left + shape.w;
        let mid = shape.h / 2.0;
        let mut out = Painted::default();
        match content {
            Content::Blank => {}
            Content::Peek => out.pet = Some(PetSpot { center: (left + shape.w / 2.0, mid), size: pet::Size::Peek }),
            Content::Pill { done, total, music, flag } => {
                let mut x = right - 14.0;
                if let Some((icon, i)) = flag {
                    self.icon(*icon, x - 6.0, mid, 13.0, 2.2, theme::AMBER)?;
                    out.hits.push((area(x - 15.0, 0.0, x + 4.0, shape.h), Hit::DeckFlag(*i)));
                    x -= 18.0;
                }
                // Moving Composition equalizer while a song plays (same as Live).
                if *music {
                    out.eq = Some((x - 6.0, mid));
                    x -= 16.0;
                }
                if *total == 0 && !*music {
                    out.pet = Some(PetSpot { center: (left + shape.w / 2.0, mid), size: pet::Size::Pill });
                } else {
                    out.pet = Some(PetSpot { center: (left + 27.0, mid), size: pet::Size::Pill });
                }
                if *total > 0 {
                    let remaining = total - done;
                    if remaining > 0 {
                        let label = remaining.to_string();
                        let w = self.width(PILL_COUNT, &label);
                        self.text(&label, PILL_COUNT, area(x - w - 2.0, 0.0, x, shape.h), white(0.75), Align::Trailing)?;
                        x -= w + 5.0;
                    }
                    self.progress_ring(x - 6.0, mid, 4.7, 2.2, *done as f32 / *total as f32, theme::LABEL)?;
                }
            }
            Content::Next { kicker, warn, title, side } => {
                out.pet = Some(PetSpot { center: (left + 30.0, mid + 1.0), size: pet::Size::Large });
                let x = left + 56.0;
                let side_w = side.as_ref().map_or(0.0, |s| self.width(SMALL, s) + 12.0);
                if let Some(s) = side {
                    self.text(s, SMALL, area(right - 18.0 - side_w, 0.0, right - 18.0, shape.h), theme::SECONDARY, Align::Trailing)?;
                }
                let text_r = right - 18.0 - side_w;
                self.text(kicker, SMALL, area(x, mid - 18.0, text_r, mid - 1.0), if *warn { theme::AMBER } else { theme::SECONDARY }, Align::Leading)?;
                self.text(title, NEXT_TITLE, area(x, mid - 2.0, text_r, mid + 17.0), theme::LABEL, Align::Leading)?;
            }
            Content::NowPlaying { title, artist, controls } => {
                out.pet = Some(PetSpot { center: (left + 30.0, mid + 1.0), size: pet::Size::Large });
                let x = left + 56.0;
                let text_r = self.media_buttons(&mut out, right - 12.0, mid, 0.0, shape.h, controls)? - 10.0;
                let kicker = if artist.is_empty() { "Now playing" } else { artist.as_str() };
                self.text(kicker, SMALL, area(x, mid - 18.0, text_r, mid - 1.0), theme::SECONDARY, Align::Leading)?;
                self.line_or_marquee(&mut out, vec![(title.clone(), NEXT_TITLE, theme::LABEL)], area(x, mid - 2.0, text_r, mid + 17.0))?;
            }
            Content::Glance { title, detail } => {
                out.pet = Some(PetSpot { center: (left + 26.0, mid), size: pet::Size::Badge });
                let x = left + 44.0;
                let title_w = self.width(GLANCE_TITLE, title);
                self.text(title, GLANCE_TITLE, area(x, 0.0, (x + title_w + 2.0).min(right - 16.0), shape.h), theme::LABEL, Align::Leading)?;
                if let Some(d) = detail {
                    let dx = x + title_w + GLANCE_GAP;
                    self.text(d, GLANCE_DETAIL, area(dx, 0.0, right - 16.0, shape.h), theme::SECONDARY, Align::Leading)?;
                }
            }
            Content::Live { activity, project, others, minutes, waiting, hover, music, controls, song } => {
                let (d, x) = if *hover { (RING_MAX, left + 60.0) } else { (22.0, left + 38.0) };
                let (cx, cy) = (left + if *hover { 12.0 } else { 8.0 } + d / 2.0, mid);
                self.orb(cx, cy, d / 2.0 - 3.0, gray(0.11, 1.0), gray(0.02, 1.0))?;
                out.pet = Some(PetSpot { center: (cx, cy), size: if *hover { pet::Size::Badge } else { pet::Size::Tiny } });
                if *waiting {
                    self.outline(cx, cy, d / 2.0 - 0.7, 1.3, theme::AMBER)?;
                } else {
                    out.rings.push(RingSpot { center: (cx, cy), diameter: d });
                }
                // Without a step number: with no caption, that number is hard to grasp at a glance.
                // Composition equalizer at the far right; text stops before it.
                let text_r = if let (true, Some(c)) = (*hover, controls) {
                    // Song column on the right: title and artist (scrolling if long), buttons below it.
                    const COL: f32 = 136.0;
                    let (col_l, col_r) = (right - 14.0 - COL, right - 14.0);
                    if let Some((title, artist)) = song {
                        let mut parts = vec![(title.clone(), TINY_STRONG, white(0.8))];
                        if !artist.is_empty() {
                            parts.push((format!("  {artist}"), TINY, theme::TERTIARY));
                        }
                        self.line_or_marquee(&mut out, parts, area(col_l, mid - 19.0, col_r, mid - 3.0))?;
                    }
                    self.media_buttons(&mut out, col_r - (COL - 78.0) / 2.0, mid + 10.0, mid - 2.0, shape.h, c)?;
                    self.line(col_l - 10.0, mid - 14.0, col_l - 10.0, mid + 14.0, theme::HAIRLINE)?;
                    col_l - 20.0
                } else if *music {
                    let ex = right - if *hover { 24.0 } else { 19.0 };
                    out.eq = Some((ex, mid));
                    ex - 16.0
                } else if *hover {
                    right - 18.0
                } else {
                    right - 14.0
                };
                if *hover {
                    // Top row: Claude logo (not the text "Claude Code"), project, other sessions, time spent working.
                    let ky = mid - 10.0;
                    self.svg(brands::CLAUDE, x + 6.0, ky, 12.0, white(0.7))?;
                    let mut kx = x + 17.0;
                    let pw = self.width(TINY_STRONG, project).min(text_r - kx - 70.0).max(0.0);
                    self.text(project, TINY_STRONG, area(kx, ky - 8.0, kx + pw + 1.0, ky + 8.0), white(0.75), Align::Leading)?;
                    kx += pw + 6.0;
                    if *others > 0 {
                        let more = format!("+{others}");
                        let w = self.width(TINY_STRONG, &more) + 10.0;
                        self.round_rect(area(kx, ky - 7.0, kx + w, ky + 7.0), 7.0, theme::TAB_FILL)?;
                        self.text(&more, TINY_STRONG, area(kx, ky - 8.0, kx + w, ky + 8.0), white(0.75), Align::Center)?;
                        kx += w + 6.0;
                    }
                    self.text(&format!("\u{00B7} {}", duration(*minutes)), TINY, area(kx, ky - 8.0, text_r, ky + 8.0), theme::TERTIARY, Align::Leading)?;
                    // Long activity scrolls like a song title instead of being clipped.
                    self.line_or_marquee(&mut out, vec![(activity.clone(), NEXT_TITLE, theme::LABEL)], area(x, mid - 2.0, text_r, mid + 17.0))?;
                } else {
                    // Dim project on the right (before the equalizer if present); more compact while music
                    // plays so the activity stays readable.
                    let tag = if *others > 0 { format!("{project} +{others}") } else { project.clone() };
                    let tag_w = self.width(TINY, &tag).min(if *music { 56.0 } else { 80.0 });
                    self.text(&tag, TINY, area(text_r - tag_w, 0.0, text_r, shape.h), theme::TERTIARY, Align::Trailing)?;
                    self.line_or_marquee(&mut out, vec![(activity.clone(), LIVE_NOW, theme::LABEL)], area(x, 0.0, text_r - 8.0 - tag_w, shape.h))?;
                }
            }
            Content::Focus { kicker, time, progress, paused, dim, hover } => {
                let ring = if *dim { white(0.5) } else { theme::LABEL };
                if *hover {
                    out.pet = Some(PetSpot { center: (left + 30.0, mid + 1.0), size: pet::Size::Large });
                    let (rcx, rr) = (right - 18.0 - 11.0, 11.0);
                    self.progress_ring(rcx, mid, rr, 2.6, *progress, ring)?;
                    if *paused {
                        self.icon(Icon::Pause, rcx, mid, 10.0, 2.2, theme::SECONDARY)?;
                    }
                    let (x, text_r) = (left + 56.0, rcx - rr - 12.0);
                    self.text(kicker, SMALL, area(x, mid - 18.0, text_r, mid - 1.0), theme::SECONDARY, Align::Leading)?;
                    self.text(time, FOCUS_HOVER_TIME, area(x, mid - 3.0, text_r, mid + 18.0), theme::LABEL, Align::Leading)?;
                } else {
                    out.pet = Some(PetSpot { center: (left + 27.0, mid), size: pet::Size::Pill });
                    let x = right - 14.0;
                    let w = self.width(PILL_COUNT, time);
                    self.text(time, PILL_COUNT, area(x - w - 2.0, 0.0, x, shape.h), white(if *dim { 0.5 } else { 0.75 }), Align::Trailing)?;
                    let cx = x - w - 11.0;
                    if *paused {
                        self.icon(Icon::Pause, cx, mid, 11.0, 2.2, theme::SECONDARY)?;
                    } else {
                        self.progress_ring(cx, mid, 4.7, 2.2, *progress, ring)?;
                    }
                }
            }
            Content::Permission { project, tool, action, queued, remaining, wait, hover, scroll } => {
                return self.paint_permission(left, right, shape.h, project, tool, action, *queued, (*remaining, *wait), *hover, *scroll)
            }
            Content::Question { project, header, text, options, multi, step, remaining, wait, hover, scroll } => {
                return self.paint_question(left, right, shape.h, project, header.as_deref(), text, options, *multi, *step, (*remaining, *wait), *hover, *scroll)
            }
            Content::Plan { project, excerpt, remaining, wait, hover, scroll } => {
                return self.paint_plan(left, right, shape.h, project, excerpt, (*remaining, *wait), *hover, *scroll)
            }
            Content::SmartChip(v) => return self.paint_chip(left, right, shape.h, v),
            Content::Expanded(panel) => return self.paint_panel(left, right, shape.h, panel),
        }
        Ok(out)
    }

    /// Compact permission card: Nochi who needs you, the requested command, round Deny /
    /// Allow buttons, and an amber line on the bottom edge that shrinks until the decision returns to Claude.
    /// Long commands wrap in full; once the card is as tall as the canvas, the box scrolls.
    #[allow(clippy::too_many_arguments)]
    fn paint_permission(&self, left: f32, right: f32, height: f32, project: &str, tool: &str, action: &str, queued: usize, time: (u64, u64), hover: Option<Hit>, scroll: f32) -> Result<Painted> {
        use layout::*;
        const M: f32 = CARD_M;
        let mut out = Painted::default();
        let tile = area(left + M, 10.0, left + M + 30.0, 40.0);
        self.tile(tile, 10.0)?;
        out.pet = Some(PetSpot { center: ((tile.l + tile.r) / 2.0, (tile.t + tile.b) / 2.0), size: pet::Size::Badge });
        self.text("!", BANG, area(tile.r - 9.0, tile.t - 9.0, tile.r + 5.0, tile.t + 7.0), theme::AMBER, Align::Center)?;

        let (cy, allow_x) = (25.0, right - M - 16.0);
        let deny_x = allow_x - 40.0;
        let x = tile.r + 10.0;
        let title = match tool {
            "Bash" | "PowerShell" => "Claude wants to run".to_string(),
            // Via MCP `request_approval`: the agent asks for approval for its own action.
            "approval" => format!("{project} needs your OK"),
            _ => format!("Claude wants to use {tool}"),
        };
        self.text(&title, PERM_TITLE, area(x, 9.0, deny_x - 22.0, 26.0), theme::LABEL, Align::Leading)?;
        let approval = tool == "approval";
        let mut sub = if approval { format!("{project} \u{00B7} asks before acting") } else { format!("{project} \u{00B7} {tool}") };
        if queued > 0 {
            sub.push_str(&format!(" \u{00B7} {queued} more waiting"));
        }
        self.text(&sub, SMALL, area(x, 25.0, deny_x - 22.0, 41.0), theme::SECONDARY, Align::Leading)?;

        let deny_fill = if hover == Some(Hit::Deny) { gray(0.23, 1.0) } else { theme::GRAPHITE };
        self.circle(deny_x, cy, 16.0, deny_fill)?;
        self.icon(Icon::Cross, deny_x, cy, 13.0, 2.4, theme::LABEL)?;
        self.circle(allow_x, cy, 16.0, if hover == Some(Hit::Allow) { white(0.86) } else { theme::LABEL })?;
        self.icon(Icon::Check, allow_x, cy, 15.0, 2.8, theme::BLACK)?;
        for (cx, hit) in [(deny_x, Hit::Deny), (allow_x, Hit::Allow)] {
            out.hits.push((area(cx - 19.0, cy - 19.0, cx + 19.0, cy + 19.0), hit));
        }

        let body = permission_body(self.dwrite, approval, action);
        self.scrolled(left, right, height, &body, scroll, |top| {
            let code = area(left + M, top + 2.0, right - M, top + body.h - 10.0);
            self.round_rect(code, 9.0, theme::FIELD)?;
            // One line centered in the box; wrapped text starts at the top with the same padding.
            let ty = code.t + ((code.b - code.t) - body.text_h) / 2.0;
            // Approval via MCP isn't necessarily a shell command: no "$" prompt.
            let text_l = if approval { code.l + 12.0 } else { code.l + 26.0 };
            if !approval {
                self.draw_text("$", MONO, area(code.l + 12.0, ty, code.l + 24.0, code.b), theme::TERTIARY, Align::Leading, true)?;
            }
            self.draw_text(action, MONO, area(text_l, ty, code.r - 12.0, code.b), theme::LABEL, Align::Leading, true)
        })?;

        // Countdown: thin line on the bottom edge, shrinking from the right.
        self.countdown(left, right, height, time)?;
        Ok(out)
    }

    /// Card content below the head. `draw` receives the content's start y (already shifted by `scroll`) and
    /// draws inside a clip; if the content is taller than its space, the clipped edge fades
    /// and a scroll line shows on the right.
    fn scrolled(&self, left: f32, right: f32, height: f32, body: &CardBody, scroll: f32, draw: impl FnOnce(f32) -> Result<()>) -> Result<()> {
        let view = area(left + 6.0, layout::CARD_HEAD, right - 6.0, height - body.foot);
        let max = (body.h - (view.b - view.t)).max(0.0);
        let scroll = scroll.clamp(0.0, max);
        unsafe { self.ctx.PushAxisAlignedClip(&rect(view), D2D1_ANTIALIAS_MODE_ALIASED) };
        let result = draw(view.t - scroll);
        unsafe { self.ctx.PopAxisAlignedClip() };
        result?;
        if scroll > 0.0 {
            self.fade(area(view.l, view.t, view.r, view.t + 12.0), false)?;
        }
        if scroll < max {
            self.fade(area(view.l, view.b - 18.0, view.r, view.b), true)?;
        }
        if max > 0.0 {
            self.scrollbar(area(view.l, view.t, view.r - 1.0, view.b), scroll / body.h, (view.b - view.t) / body.h)?;
        }
        Ok(())
    }

    /// Head of a card awaiting you: wide-eyed Nochi on a tile with an amber mark, title, and
    /// description. Returns the text x position.
    fn card_head(&self, out: &mut Painted, left: f32, mark: &str, title: &str, sub: &str, text_r: f32) -> Result<f32> {
        let tile = area(left + 14.0, 10.0, left + 44.0, 40.0);
        self.tile(tile, 10.0)?;
        out.pet = Some(PetSpot { center: ((tile.l + tile.r) / 2.0, (tile.t + tile.b) / 2.0), size: pet::Size::Badge });
        self.text(mark, BANG, area(tile.r - 9.0, tile.t - 9.0, tile.r + 5.0, tile.t + 7.0), theme::AMBER, Align::Center)?;
        let x = tile.r + 10.0;
        self.text(title, PERM_TITLE, area(x, 9.0, text_r, 26.0), theme::LABEL, Align::Leading)?;
        self.text(sub, SMALL, area(x, 25.0, text_r, 41.0), theme::SECONDARY, Align::Leading)?;
        Ok(x)
    }

    /// Text pill button. `primary`: white with black text; otherwise graphite.
    fn pill_button(&self, out: &mut Painted, a: Area, label: &str, primary: bool, hit: Hit, hover: Option<Hit>) -> Result<()> {
        let hovered = hover == Some(hit);
        let fill = match (primary, hovered) {
            (true, false) => theme::LABEL,
            (true, true) => white(0.86),
            (false, false) => theme::GRAPHITE,
            (false, true) => gray(0.23, 1.0),
        };
        self.round_rect(a, (a.b - a.t) / 2.0, fill)?;
        self.text(label, TINY_STRONG, a, if primary { theme::BLACK } else { theme::LABEL }, Align::Center)?;
        out.hits.push((a, hit));
        Ok(())
    }

    /// Amber countdown line on the bottom edge of the card.
    fn countdown(&self, left: f32, right: f32, height: f32, (remaining, wait): (u64, u64)) -> Result<()> {
        let (l, r, y) = (left + 26.0, right - 26.0, height - 3.0);
        self.round_rect(area(l, y, r, y + 2.0), 1.0, white(0.08))?;
        let fraction = (remaining as f32 / wait.max(1) as f32).clamp(0.0, 1.0);
        if fraction > 0.0 {
            self.round_rect(area(l, y, l + (r - l) * fraction, y + 2.0), 1.0, theme::AMBER)?;
        }
        Ok(())
    }

    /// Choice-question card from the agent. Options are picked right in the notch; "In app" returns the
    /// question to the agent UI (e.g. for a free-form answer). A single-answer question uses radio
    /// buttons and is answered on click; multi-select uses checkboxes and a Done button.
    #[allow(clippy::too_many_arguments)]
    fn paint_question(
        &self,
        left: f32,
        right: f32,
        height: f32,
        project: &str,
        header: Option<&str>,
        text: &str,
        options: &[(String, Option<String>, bool)],
        multi: bool,
        step: (usize, usize),
        time: (u64, u64),
        hover: Option<Hit>,
        scroll: f32,
    ) -> Result<Painted> {
        use layout::*;
        const M: f32 = CARD_M;
        let mut out = Painted::default();
        let in_app = area(right - M - 64.0, 13.0, right - M, 37.0);
        self.pill_button(&mut out, in_app, "In app", false, Hit::InApp, hover)?;
        let mut sub = project.to_string();
        if let Some(h) = header {
            sub = format!("{sub} \u{00B7} {h}");
        }
        if step.1 > 1 {
            sub = format!("{sub} \u{00B7} {} of {}", step.0, step.1);
        }
        let title = if multi { "Claude asks \u{00B7} pick any" } else { "Claude asks" };
        self.card_head(&mut out, left, "?", title, &sub, in_app.l - 8.0)?;

        let body = question_body(self.dwrite, text, options, multi);
        let view = area(left, CARD_HEAD, right, height - body.foot);
        let mut inner = Painted::default();
        self.scrolled(left, right, height, &body, scroll, |top| {
            self.draw_text(text, ROW_STRONG, area(left + M, top, right - M, top + body.text_h + 2.0), theme::LABEL, Align::Leading, true)?;
            let mut y = top + body.text_h + 8.0;
            for (i, ((label, detail, picked), &(h, stacked))) in options.iter().zip(&body.rows).enumerate() {
                let row = area(left + M, y, right - M, y + h);
                let hovered = hover == Some(Hit::Choice(i));
                self.round_rect(row, 10.0, if hovered { gray(0.15, 1.0) } else { theme::FIELD })?;
                // Mark centered on the first row: checkbox (multi) or radio button.
                let (cx, cy) = (row.l + 17.0, y + OPTION_H / 2.0);
                match (multi, *picked) {
                    (true, true) => {
                        self.round_rect(area(cx - 6.5, cy - 6.5, cx + 6.5, cy + 6.5), 3.5, theme::LABEL)?;
                        self.icon(Icon::Check, cx, cy, 9.0, 1.8, theme::BLACK)?;
                    }
                    (true, false) => self.round_outline(area(cx - 6.0, cy - 6.0, cx + 6.0, cy + 6.0), 3.0, 1.3, white(if hovered { 0.7 } else { 0.4 }))?,
                    (false, _) => {
                        self.outline(cx, cy, 6.0, 1.3, white(if hovered { 0.7 } else { 0.4 }))?;
                        if hovered {
                            self.circle(cx, cy, 3.0, theme::LABEL)?;
                        }
                    }
                }
                let x = row.l + OPTION_TEXT_L;
                match stacked {
                    None => {
                        let label_w = self.width(TINY_STRONG, label);
                        self.text(label, TINY_STRONG, area(x, y, x + label_w + 2.0, y + OPTION_H), theme::LABEL, Align::Leading)?;
                        if let Some(d) = detail {
                            self.text(d, TINY, area(x + label_w + 8.0, y, row.r - 10.0, y + OPTION_H), theme::TERTIARY, Align::Leading)?;
                        }
                    }
                    Some(label_h) => {
                        let ty = y + OPTION_PAD;
                        self.draw_text(label, TINY_STRONG, area(x, ty, x + OPTION_TEXT_W, ty + label_h + 2.0), theme::LABEL, Align::Leading, true)?;
                        if let Some(d) = detail {
                            self.draw_text(d, TINY, area(x, ty + label_h + 1.0, x + OPTION_TEXT_W, row.b), theme::TERTIARY, Align::Leading, true)?;
                        }
                    }
                }
                inner.hits.push((row, Hit::Choice(i)));
                y += h + OPTION_GAP;
            }
            if multi {
                let n = options.iter().filter(|o| o.2).count();
                let note = if n == 0 { "Pick one or more".to_string() } else { format!("{n} selected") };
                self.text(&note, TINY, area(left + M + 4.0, y, right - M - 100.0, y + OPTION_H), theme::TERTIARY, Align::Leading)?;
                let done = area(right - M - 88.0, y, right - M, y + OPTION_H);
                if n > 0 {
                    self.pill_button(&mut inner, done, "Done", true, Hit::Done, hover)?;
                } else {
                    self.round_rect(done, OPTION_H / 2.0, theme::GRAPHITE)?;
                    self.text("Done", TINY_STRONG, done, theme::TERTIARY, Align::Center)?;
                }
            }
            Ok(())
        })?;
        // Only the visible part of the row is clickable.
        out.hits.extend(inner.hits.into_iter().filter(|(a, _)| a.b > view.t && a.t < view.b).map(|(a, h)| (area(a.l, a.t.max(view.t), a.r, a.b.min(view.b)), h)));
        self.countdown(left, right, height, time)?;
        Ok(out)
    }

    /// Plan card awaiting approval: the full plan (scrolls if long) and three buttons fixed at the bottom.
    #[allow(clippy::too_many_arguments)]
    fn paint_plan(&self, left: f32, right: f32, height: f32, project: &str, excerpt: &str, time: (u64, u64), hover: Option<Hit>, scroll: f32) -> Result<Painted> {
        use layout::*;
        const M: f32 = CARD_M;
        let mut out = Painted::default();
        self.card_head(&mut out, left, "!", "Plan ready for review", project, right - M)?;
        let body = plan_body(self.dwrite, excerpt);
        self.scrolled(left, right, height, &body, scroll, |top| {
            self.draw_text(excerpt, SMALL, area(left + M, top + 2.0, right - M, top + 2.0 + body.text_h + 2.0), theme::SECONDARY, Align::Leading, true)
        })?;
        let y = height - 16.0 - OPTION_H;
        let w = (right - left - 2.0 * M - 16.0) / 3.0;
        let at = |i: f32| area(left + M + i * (w + 8.0), y, left + M + i * (w + 8.0) + w, y + OPTION_H);
        self.pill_button(&mut out, at(0.0), "Review in app", false, Hit::InApp, hover)?;
        self.pill_button(&mut out, at(1.0), "Keep planning", false, Hit::Deny, hover)?;
        self.pill_button(&mut out, at(2.0), "Approve", true, Hit::Allow, hover)?;
        self.countdown(left, right, height, time)?;
        Ok(out)
    }

    /// Draws a bitmap (e.g. app icon) filling `a` as-is.
    fn bitmap(&self, a: Area, t: &Thumb) -> Result<()> {
        let rt: &ID2D1RenderTarget = self.ctx;
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            let bmp = rt.CreateBitmap(D2D_SIZE_U { width: t.w, height: t.h }, Some(t.bgra.as_ptr() as *const _), t.w * 4, &props)?;
            rt.DrawBitmap(&bmp, Some(&rect(a)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
        }
        Ok(())
    }

    /// One deck button: tile with app icon or line icon, hotkey number, on marker (amber), and label.
    fn deck_key(&self, out: &mut Painted, cell: Area, k: &DeckKey, hit: Hit, hover: Option<Hit>) -> Result<()> {
        if hover == Some(hit) {
            self.round_rect(cell, 12.0, white(0.05))?;
        }
        let cx = (cell.l + cell.r) / 2.0;
        let cap = area(cx - 21.0, cell.t + 4.0, cx + 21.0, cell.t + 46.0);
        self.tile(cap, 13.0)?;
        let ink = if k.on { theme::AMBER } else { theme::LABEL };
        match &k.image {
            Some(img) => self.bitmap(area(cx - 14.0, cap.t + 7.0, cx + 14.0, cap.t + 35.0), img)?,
            None => self.icon(k.glyph, cx, (cap.t + cap.b) / 2.0, 18.0, 2.0, ink)?,
        }
        if k.on {
            self.edge(cap, 13.0, theme::AMBER)?;
            self.circle(cap.r - 1.0, cap.t + 1.0, 4.5, theme::BLACK)?;
            self.circle(cap.r - 1.0, cap.t + 1.0, 3.2, theme::AMBER)?;
        }
        if let Some(n) = k.number {
            let label = n.to_string();
            let w = self.width(BADGE, &label) + 8.0;
            let b = area(cap.l - 3.0, cap.t - 3.0, cap.l - 3.0 + w, cap.t + 10.0);
            self.round_rect(b, 5.0, theme::TAB_FILL)?;
            self.text(&label, BADGE, b, white(0.75), Align::Center)?;
        }
        self.text(&k.label, DECK_LABEL, area(cell.l + 1.0, cell.t + 48.0, cell.r - 1.0, cell.t + 62.0), white(0.72), Align::Center)?;
        out.hits.push((cell, hit));
        Ok(())
    }

    /// Five-column deck grid starting at `y`, at most `rows` rows; Add box at the end if it still fits.
    #[allow(clippy::too_many_arguments)]
    fn deck_grid(&self, out: &mut Painted, l: f32, r: f32, y: f32, g: &DeckGrid, rows: usize, hover: Option<Hit>) -> Result<()> {
        use layout::{DECK_COLS, DECK_KEY_H};
        let cell_w = (r - l) / DECK_COLS as f32;
        let cell = |i: usize| {
            let (col, row) = (i % DECK_COLS, i / DECK_COLS);
            let x = l + col as f32 * cell_w;
            let t = y + row as f32 * DECK_KEY_H;
            area(x + 1.0, t, x + cell_w - 1.0, t + DECK_KEY_H - 2.0)
        };
        let max = rows * DECK_COLS;
        for (i, k) in g.keys.iter().enumerate().take(max) {
            self.deck_key(out, cell(i), k, Hit::DeckKey(g.first + i), hover)?;
        }
        if g.add && g.keys.len() < max {
            let c = cell(g.keys.len());
            if hover == Some(Hit::DeckAdd) {
                self.round_rect(c, 12.0, white(0.05))?;
            }
            let cx = (c.l + c.r) / 2.0;
            let cap = area(cx - 21.0, c.t + 4.0, cx + 21.0, c.t + 46.0);
            self.edge(cap, 13.0, white(0.22))?;
            self.icon(Icon::Plus, cx, (cap.t + cap.b) / 2.0, 15.0, 2.2, white(0.6))?;
            self.text("Add", DECK_LABEL, area(c.l, c.t + 48.0, c.r, c.t + 62.0), white(0.6), Align::Center)?;
            out.hits.push((c, Hit::DeckAdd));
        }
        Ok(())
    }

    /// Deck tab: three rows per page and page dots.
    fn paint_deck(&self, out: &mut Painted, left: f32, right: f32, d: &DeckPage, hover: Option<Hit>) -> Result<()> {
        use layout::*;
        self.deck_grid(out, left + 10.0, right - 10.0, HEAD + 4.0, &d.grid, DECK_ROWS, hover)?;
        if d.pages > 1 {
            let y = HEAD + 4.0 + DECK_ROWS as f32 * DECK_KEY_H + 6.0;
            let w = d.pages as f32 * 12.0 - 6.0;
            let mut x = (left + right) / 2.0 - w / 2.0 + 3.0;
            for p in 0..d.pages {
                self.circle(x, y, 3.0, if p == d.page { theme::LABEL } else { white(0.2) })?;
                x += 12.0;
            }
        } else if d.grid.keys.is_empty() {
            let y = HEAD + 4.0 + DECK_KEY_H;
            self.text("Right-click a button to change it. Ctrl+Alt+1\u{2013}0 runs the first ten.", SMALL, area(left + 18.0, y + 6.0, right - 18.0, y + 26.0), white(0.55), Align::Leading)?;
        }
        Ok(())
    }

    /// Field in the editor: its content, or a hint when empty; the EDIT window is placed over it when active.
    fn edit_field(&self, out: &mut Painted, a: Area, f: &EditField, hover: Option<Hit>) -> Result<()> {
        let hit = Hit::Edit(EditCmd::Field(f.id));
        self.round_rect(a, 10.0, theme::FIELD)?;
        self.edge(a, 10.0, white(if f.active { 0.18 } else if hover == Some(hit) { 0.12 } else { 0.07 }))?;
        let inner = area(a.l + 10.0, a.t + 5.0, a.r - 8.0, a.b - 5.0);
        if f.active {
            out.input = Some(inner);
            return Ok(());
        }
        match &f.keycaps {
            Some(caps) if !caps.is_empty() => {
                let mut x = inner.l;
                for c in caps {
                    let w = self.width(TINY_STRONG, c) + 12.0;
                    let k = area(x, a.t + 7.0, x + w, a.b - 7.0);
                    self.round_rect(k, 5.0, gray(0.17, 1.0))?;
                    self.text(c, TINY_STRONG, k, theme::LABEL, Align::Center)?;
                    x = k.r + 4.0;
                }
            }
            _ if f.value.is_empty() => self.text(f.placeholder, FIELD, inner, white(0.4), Align::Leading)?,
            _ => self.text(&f.value, FIELD, inner, theme::LABEL, Align::Leading)?,
        }
        out.hits.push((a, hit));
        Ok(())
    }

    /// Segmented control; returns the area of each segment.
    fn segments(&self, out: &mut Painted, a: Area, labels: &[(&str, bool, Hit)], hover: Option<Hit>) -> Result<()> {
        self.round_rect(a, 11.0, theme::FIELD)?;
        let w = (a.r - a.l - 4.0) / labels.len() as f32;
        for (i, (label, on, hit)) in labels.iter().enumerate() {
            let s = area(a.l + 2.0 + i as f32 * w, a.t + 2.0, a.l + 2.0 + (i + 1) as f32 * w, a.b - 2.0);
            if *on {
                self.round_rect(s, 9.0, gray(0.17, 1.0))?;
            } else if hover == Some(*hit) {
                self.round_rect(s, 9.0, white(0.05))?;
            }
            self.text(label, TINY_STRONG, s, if *on { theme::LABEL } else { white(0.6) }, Align::Center)?;
            out.hits.push((s, *hit));
        }
        Ok(())
    }

    /// Editor for one deck button.
    fn paint_deck_editor(&self, out: &mut Painted, left: f32, right: f32, height: f32, e: &DeckEditor, hover: Option<Hit>) -> Result<()> {
        use layout::*;
        const M: f32 = 14.0;
        let (l, r) = (left + M, right - M);
        // Header: back, title, delete.
        let back = area(l - 4.0, 9.0, l + 22.0, 35.0);
        if hover == Some(Hit::Edit(EditCmd::Back)) {
            self.round_rect(back, 13.0, white(0.08))?;
        }
        self.icon(Icon::ChevronLeft, back.l + 13.0, 22.0, 14.0, 2.4, theme::LABEL)?;
        out.hits.push((back, Hit::Edit(EditCmd::Back)));
        self.text(&e.title, ROW_STRONG, area(back.r + 6.0, 9.0, r - 40.0, 35.0), theme::LABEL, Align::Leading)?;
        if e.can_delete {
            let del = area(r - 26.0, 9.0, r, 35.0);
            if hover == Some(Hit::Edit(EditCmd::Delete)) {
                self.round_rect(del, 13.0, white(0.08))?;
            }
            self.icon(Icon::Trash, del.l + 13.0, 22.0, 14.0, 2.0, white(0.6))?;
            out.hits.push((del, Hit::Edit(EditCmd::Delete)));
        }
        // Icon (click to change) and label.
        let key = DeckKey { label: String::new(), glyph: e.glyph, image: e.image.clone(), number: None, on: e.toggle };
        let cap = area(l - 4.0, 44.0, l + 46.0, 106.0);
        self.deck_key(out, cap, &key, Hit::Edit(EditCmd::Glyph), hover)?;
        self.edit_field(out, area(l + 56.0, 52.0, r, 84.0), &e.label, hover)?;
        // Action type.
        let kinds: Vec<(&str, bool, Hit)> = deck::Kind::ALL.iter().map(|k| (k.label(), *k == e.kind, Hit::Edit(EditCmd::Kind(*k)))).collect();
        self.segments(out, area(l, 94.0, r, 120.0), &kinds, hover)?;
        // Field matching the type.
        let mut y = 126.0;
        let caption = |s: &str, y: f32| self.text(s, TINY, area(l, y, l + 64.0, y + EDIT_ROW_H), white(0.55), Align::Leading);
        if e.kind == deck::Kind::Notch {
            caption("Action", y)?;
            let kinds: Vec<(&str, bool, Hit)> = deck::NotchKind::ALL.iter().map(|k| (k.label(), *k == e.notch, Hit::Edit(EditCmd::Notch(*k)))).collect();
            self.segments(out, area(l + 68.0, y + 3.0, r, y + EDIT_ROW_H - 3.0), &kinds, hover)?;
            y += EDIT_ROW_H;
        }
        for f in &e.fields {
            caption(f.caption, y)?;
            let mut a = area(l + 68.0, y + 2.0, r, y + EDIT_ROW_H - 2.0);
            if e.kind == deck::Kind::Keys && f.id == 1 {
                // Record: press the combination, the notch writes it out.
                let rec = area(a.r - 76.0, a.t + 4.0, a.r - 4.0, a.b - 4.0);
                a.r = rec.l - 6.0;
                let (fill, ink) = if e.recording { (theme::AMBER, theme::BLACK) } else if hover == Some(Hit::Edit(EditCmd::Record)) { (gray(0.23, 1.0), theme::LABEL) } else { (theme::GRAPHITE, theme::LABEL) };
                self.round_rect(rec, (rec.b - rec.t) / 2.0, fill)?;
                if !e.recording {
                    self.circle(rec.l + 12.0, (rec.t + rec.b) / 2.0, 3.0, theme::AMBER)?;
                }
                let text = if e.recording { "Press keys" } else { "Record" };
                self.text(text, TINY_STRONG, area(rec.l + if e.recording { 0.0 } else { 18.0 }, rec.t, rec.r, rec.b), ink, if e.recording { Align::Center } else { Align::Leading })?;
                out.hits.push((rec, Hit::Edit(EditCmd::Record)));
            }
            self.edit_field(out, a, f, hover)?;
            y += EDIT_ROW_H;
        }
        // On/off state.
        caption("Shows", y)?;
        let row = area(l + 68.0, y, r, y + EDIT_ROW_H);
        self.text("On / off state, marked in the pill", SMALL, area(row.l, y, row.r - 40.0, y + EDIT_ROW_H), white(0.75), Align::Leading)?;
        self.switch(r, y + EDIT_ROW_H / 2.0, e.toggle, true)?;
        out.hits.push((row, Hit::Edit(EditCmd::State)));
        y += EDIT_ROW_H;
        caption("Hotkey", y)?;
        match &e.hotkey {
            Some(h) => {
                let mut x = l + 68.0;
                for c in h.split('+') {
                    let w = self.width(TINY_STRONG, c) + 12.0;
                    let k = area(x, y + 8.0, x + w, y + EDIT_ROW_H - 8.0);
                    self.round_rect(k, 5.0, gray(0.17, 1.0))?;
                    self.text(c, TINY_STRONG, k, theme::LABEL, Align::Center)?;
                    x = k.r + 4.0;
                }
            }
            None => self.text("None, buttons after the tenth run from the deck", SMALL, area(l + 68.0, y, r, y + EDIT_ROW_H), white(0.5), Align::Leading)?,
        }
        // Bottom: error message, Cancel, Save.
        let by = height - 12.0 - 28.0;
        let save = area(r - 64.0, by, r, by + 28.0);
        let cancel = area(save.l - 8.0 - 72.0, by, save.l - 8.0, by + 28.0);
        self.pill_button(out, cancel, "Cancel", false, Hit::Edit(EditCmd::Back), hover)?;
        self.pill_button(out, save, "Save", true, Hit::Edit(EditCmd::Save), hover)?;
        if let Some(err) = &e.error {
            self.text(err, TINY_STRONG, area(l, by, cancel.l - 8.0, by + 28.0), theme::AMBER, Align::Leading)?;
        }
        Ok(())
    }

    /// Thin border inside a rounded rectangle.
    fn edge(&self, a: Area, radius: f32, color: D2D1_COLOR_F) -> Result<()> {
        let rr = D2D1_ROUNDED_RECT { rect: rect(area(a.l + 0.5, a.t + 0.5, a.r - 0.5, a.b - 0.5)), radiusX: radius - 0.5, radiusY: radius - 0.5 };
        unsafe { self.ctx.DrawRoundedRectangle(&rr, &self.brush(color)?, 1.0, None) };
        Ok(())
    }

    /// Small keyboard keycap, e.g. "Esc" and "↵".
    fn key_cap(&self, a: Area, label: &str) -> Result<()> {
        self.edge(a, 6.0, white(0.16))?;
        self.text(label, TINY, a, white(0.62), Align::Center)
    }

    /// Thumbnail image filling `a` (cropped at the center), with rounded corners.
    fn thumb(&self, a: Area, t: &Thumb) -> Result<()> {
        let rt: &ID2D1RenderTarget = self.ctx;
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            let bmp = rt.CreateBitmap(D2D_SIZE_U { width: t.w, height: t.h }, Some(t.bgra.as_ptr() as *const _), t.w * 4, &props)?;
            let brush_props = D2D1_BITMAP_BRUSH_PROPERTIES {
                extendModeX: D2D1_EXTEND_MODE_CLAMP,
                extendModeY: D2D1_EXTEND_MODE_CLAMP,
                interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            };
            let brush = rt.CreateBitmapBrush(&bmp, Some(&brush_props), None)?;
            let (aw, ah) = (a.r - a.l, a.b - a.t);
            let k = (aw / t.w as f32).max(ah / t.h as f32);
            let (dx, dy) = (a.l + (aw - t.w as f32 * k) / 2.0, a.t + (ah - t.h as f32 * k) / 2.0);
            brush.SetTransform(&(Matrix3x2::scale(k, k) * Matrix3x2::translation(dx, dy)));
            let rr = D2D1_ROUNDED_RECT { rect: rect(a), radiusX: 7.0, radiusY: 7.0 };
            self.ctx.FillRoundedRectangle(&rr, &brush);
        }
        self.edge(a, 7.0, white(0.18))
    }

    /// Chip and command bar card button: with text (optional icon on the left) or icon only.
    #[allow(clippy::too_many_arguments)]
    fn action_button(&self, out: &mut Painted, a: Area, label: Option<&str>, icon: Option<Icon>, style: ButtonStyle, hit: Hit, hover: Option<Hit>) -> Result<()> {
        let hovered = hover == Some(hit);
        let fill = match (style, hovered) {
            (ButtonStyle::Plain, false) => theme::GRAPHITE,
            (ButtonStyle::Plain, true) => gray(0.23, 1.0),
            (ButtonStyle::Primary, false) => theme::LABEL,
            (ButtonStyle::Primary, true) => white(0.86),
            (ButtonStyle::Amber, false) => theme::AMBER,
            (ButtonStyle::Amber, true) => with_alpha(theme::AMBER, 0.85),
        };
        let ink = if style == ButtonStyle::Plain { theme::LABEL } else { theme::BLACK };
        self.round_rect(a, (a.b - a.t) / 2.0, fill)?;
        let mid = (a.t + a.b) / 2.0;
        match (label, icon) {
            (Some(l), Some(i)) => {
                self.icon(i, a.l + 17.0, mid, 12.0, 2.2, ink)?;
                self.text(l, TINY_STRONG, area(a.l + 27.0, a.t, a.r - 8.0, a.b), ink, Align::Leading)?;
            }
            (Some(l), None) => self.text(l, TINY_STRONG, a, ink, Align::Center)?,
            (None, Some(i)) => self.icon(i, (a.l + a.r) / 2.0, mid, 13.0, 2.2, ink)?,
            (None, None) => {}
        }
        out.hits.push((a, hit));
        Ok(())
    }

    /// Button width for `action_button`.
    fn button_width(&self, label: Option<&str>, icon: Option<Icon>) -> f32 {
        match label {
            Some(l) => self.width(TINY_STRONG, l) + if icon.is_some() { 38.0 } else { 24.0 },
            None => 28.0,
        }
    }

    /// Smart chip: Nochi, icon, or thumbnail on the left; two rows of text; button on the right.
    fn paint_chip(&self, left: f32, right: f32, height: f32, v: &ChipView) -> Result<Painted> {
        let mut out = Painted::default();
        let mid = height / 2.0;
        let mut x = left + 12.0;
        match &v.lead {
            Lead::Pet | Lead::Icon(_) | Lead::Moon => {
                let (cx, r) = (x + 15.0, 15.0);
                self.orb(cx, mid, r, gray(0.17, 1.0), gray(0.04, 1.0))?;
                self.outline(cx, mid, r - 0.5, 1.0, white(0.14))?;
                match &v.lead {
                    Lead::Icon(i) => self.icon(*i, cx, mid, 14.0, 2.2, theme::LABEL)?,
                    // Drawn and moved by island above this orb.
                    Lead::Moon => out.moon = Some(MoonSpot { center: (cx, mid), size: 18.0, glow: true }),
                    _ => out.pet = Some(PetSpot { center: (cx, mid), size: pet::Size::Badge }),
                }
                x += 2.0 * r;
            }
            Lead::Thumb(t) => {
                self.thumb(area(x, mid - 15.0, x + 46.0, mid + 15.0), t)?;
                x += 46.0;
            }
        }
        let mut r = right - 12.0;
        for (i, b) in v.buttons.iter().enumerate().rev() {
            let w = self.button_width(b.label.as_deref(), b.icon);
            let a = area(r - w, mid - 14.0, r, mid + 14.0);
            self.action_button(&mut out, a, b.label.as_deref(), b.icon, b.style, Hit::Chip(i), v.hover)?;
            r = a.l - 6.0;
        }
        let (tx, tr) = (x + 10.0, r - 6.0);
        let (font, color) = if v.warn { (TINY_STRONG, theme::AMBER) } else { (TINY, white(0.6)) };
        self.text(&v.kicker, font, area(tx, mid - 17.0, tr, mid - 1.0), color, Align::Leading)?;
        self.text(&v.title, ROW_STRONG, area(tx, mid - 2.0, tr, mid + 16.0), theme::LABEL, Align::Leading)?;
        Ok(out)
    }

    /// Command bar: typing field on top, card for recognized commands, result or
    /// suggestion rows, pinned shortcuts, and key hints.
    fn paint_command(&self, left: f32, right: f32, v: &CommandView, hover: Option<Hit>) -> Result<Painted> {
        use layout::*;
        let mut out = Painted::default();
        let field = area(left + 10.0, CMD_FIELD_T, right - 10.0, CMD_FIELD_T + CMD_FIELD_H);
        self.round_rect(field, 14.0, theme::FIELD)?;
        self.edge(field, 14.0, white(0.08))?;
        let mid = (field.t + field.b) / 2.0;
        let (ox, orb_r) = (field.l + 20.0, 11.0);
        self.orb(ox, mid, orb_r, gray(0.17, 1.0), gray(0.04, 1.0))?;
        self.outline(ox, mid, orb_r - 0.5, 1.0, white(0.14))?;
        out.pet = Some(PetSpot { center: (ox, mid), size: pet::Size::Tiny });
        let esc_w = self.width(TINY, "Esc") + 12.0;
        let esc = area(field.r - 8.0 - esc_w, mid - 9.0, field.r - 8.0, mid + 9.0);
        self.key_cap(esc, "Esc")?;
        let text = area(ox + orb_r + 9.0, field.t + 6.0, esc.l - 8.0, field.b - 6.0);
        // The EDIT window is placed here; while empty it is only as wide as the text cursor, so this hint is visible.
        out.input = Some(text);
        if v.empty {
            self.text("Type a task, app, timer or sum\u{2026}", FIELD, area(text.l + 4.0, text.t, text.r, text.b), white(0.5), Align::Leading)?;
        }

        let mut y = CMD_BODY;
        let offset = v.card.is_some() as usize;
        if let Some(card) = &v.card {
            let h = cmd_card_height(card);
            self.paint_cmd_card(&mut out, area(left + 12.0, y, right - 12.0, y + h), card, v.selected == 0, hover)?;
            y += h + CMD_CARD_GAP;
        }
        for (i, row) in v.rows.iter().enumerate() {
            if let Some(section) = row.section {
                self.text(&section.to_uppercase(), SECTION, area(left + 18.0, y, right - 18.0, y + CMD_SECTION_H), white(0.55), Align::Leading)?;
                y += CMD_SECTION_H;
            }
            let n = offset + i;
            let a = area(left + 8.0, y, right - 8.0, y + CMD_ROW_H);
            let cy = y + CMD_ROW_H / 2.0;
            let selected = v.selected == n;
            let hovered = hover == Some(Hit::Cmd(n)) || hover == Some(Hit::PinToggle(i));
            if selected {
                self.round_rect(a, 11.0, theme::TAB_FILL)?;
            } else if hovered {
                self.round_rect(a, 11.0, white(0.05))?;
            }
            let tile = area(a.l + 8.0, cy - 13.0, a.l + 34.0, cy + 13.0);
            self.tile(tile, 8.0)?;
            match row.letter {
                Some(c) => self.text(&c.to_string(), TINY_STRONG, tile, theme::LABEL, Align::Center)?,
                None => self.icon(row.icon, (tile.l + tile.r) / 2.0, cy, 14.0, 2.0, theme::LABEL)?,
            }
            let mut tr = a.r - 10.0;
            if selected {
                let k = area(tr - 22.0, cy - 9.0, tr, cy + 9.0);
                self.key_cap(k, "\u{21B5}")?;
                tr = k.l - 8.0;
            }
            match row.pinned {
                Some(pinned) if hovered => {
                    let p = area(tr - 24.0, cy - 12.0, tr, cy + 12.0);
                    if hover == Some(Hit::PinToggle(i)) {
                        self.round_rect(p, 12.0, white(0.1))?;
                    }
                    self.icon(Icon::Pin, (p.l + p.r) / 2.0, cy, 13.0, 1.9, if pinned { theme::LABEL } else { theme::SECONDARY })?;
                    out.hits.push((p, Hit::PinToggle(i)));
                    tr = p.l - 6.0;
                }
                _ if !row.side.is_empty() => {
                    let w = self.width(SMALL, &row.side).min(140.0);
                    self.text(&row.side, SMALL, area(tr - w - 2.0, y, tr, y + CMD_ROW_H), white(0.55), Align::Trailing)?;
                    tr -= w + 12.0;
                }
                _ => {}
            }
            self.text(&row.title, ROW, area(tile.r + 10.0, y, tr, y + CMD_ROW_H), theme::LABEL, Align::Leading)?;
            out.hits.push((a, Hit::Cmd(n)));
            y += CMD_ROW_H;
        }
        if let Some(g) = &v.deck {
            self.text("DECK", SECTION, area(left + 18.0, y, right - 18.0, y + CMD_SECTION_H), white(0.55), Align::Leading)?;
            self.text("Ctrl+Alt+1\u{2013}0", TINY, area(left + 18.0, y, right - 18.0, y + CMD_SECTION_H), white(0.45), Align::Trailing)?;
            y += CMD_SECTION_H;
            let rows = deck_rows(g, CMD_DECK_ROWS);
            self.deck_grid(&mut out, left + 10.0, right - 10.0, y, g, rows, hover)?;
            y += rows as f32 * DECK_KEY_H;
        }

        self.line(left + 16.0, y + 0.5, right - 16.0, y + 0.5, theme::HAIRLINE)?;
        let mut hx = left + 18.0;
        for (key, label) in &v.hints {
            let kw = self.width(TINY_STRONG, key);
            self.text(key, TINY_STRONG, area(hx, y, hx + kw + 1.0, y + CMD_FOOT_H), white(0.85), Align::Leading)?;
            let lw = self.width(TINY, label);
            self.text(label, TINY, area(hx + kw + 5.0, y, hx + kw + 6.0 + lw, y + CMD_FOOT_H), white(0.55), Align::Leading)?;
            hx += kw + lw + 20.0;
        }
        Ok(out)
    }

    /// Card above command bar results: timer, count, Claude, or task.
    fn paint_cmd_card(&self, out: &mut Painted, a: Area, card: &CmdCard, selected: bool, hover: Option<Hit>) -> Result<()> {
        self.round_rect(a, 16.0, gray(0.067, 1.0))?;
        self.edge(a, 16.0, white(if selected { 0.16 } else { 0.07 }))?;
        let mid = (a.t + a.b) / 2.0;
        let (x, pad) = (a.l + 14.0, 12.0);
        let button = |label: &str, icon: Option<Icon>, style: ButtonStyle, out: &mut Painted| -> Result<f32> {
            let w = self.button_width(Some(label), icon);
            let b = area(a.r - pad - w, mid - 14.0, a.r - pad, mid + 14.0);
            self.action_button(out, b, Some(label), icon, style, Hit::Cmd(0), hover)?;
            Ok(b.l - 12.0)
        };
        match card {
            CmdCard::Timer { focus, time, detail } => {
                let tr = button("Start", None, if *focus { ButtonStyle::Amber } else { ButtonStyle::Primary }, out)?;
                let (kicker, color) = if *focus { ("Focus session", theme::AMBER) } else { ("Timer", white(0.6)) };
                self.text(kicker, TINY_STRONG, area(x, a.t + 10.0, tr, a.t + 26.0), color, Align::Leading)?;
                self.text(time, CMD_BIG, area(x, a.t + 24.0, tr, a.t + 58.0), theme::LABEL, Align::Leading)?;
                self.text(detail, SMALL, area(x, a.t + 58.0, tr, a.t + 76.0), white(0.6), Align::Leading)?;
            }
            CmdCard::Calc { value, expr } => {
                let tr = button("Copy", Some(Icon::Copy), ButtonStyle::Primary, out)?;
                self.text(value, CMD_BIG, area(x, a.t + 8.0, tr, a.t + 42.0), theme::LABEL, Align::Leading)?;
                self.text(expr, SMALL, area(x, a.t + 42.0, tr, a.t + 60.0), white(0.6), Align::Leading)?;
            }
            CmdCard::Claude { prompt, project, can_switch } => {
                let tr = button("Send", None, ButtonStyle::Primary, out)?;
                let tile = area(x - 2.0, a.t + 12.0, x + 28.0, a.t + 42.0);
                self.tile(tile, 9.0)?;
                self.svg(brands::CLAUDE, (tile.l + tile.r) / 2.0, (tile.t + tile.b) / 2.0, 15.0, theme::LABEL)?;
                let tx = tile.r + 12.0;
                self.text("Ask Claude Code", ROW_STRONG, area(tx, a.t + 10.0, tr, a.t + 28.0), theme::LABEL, Align::Leading)?;
                self.text(&format!("\u{201C}{prompt}\u{201D}"), SMALL, area(tx, a.t + 28.0, tr, a.t + 46.0), white(0.6), Align::Leading)?;
                let label = if *can_switch { format!("{project} \u{25BE}") } else { project.clone() };
                let w = self.width(TINY_STRONG, &label) + 18.0;
                let chip = area(tx, a.t + 54.0, tx + w, a.t + 76.0);
                let on = hover == Some(Hit::CmdProject);
                self.round_rect(chip, 11.0, if on { gray(0.2, 1.0) } else { theme::TAB_FILL })?;
                self.text(&label, TINY_STRONG, chip, theme::LABEL, Align::Center)?;
                if *can_switch {
                    out.hits.push((chip, Hit::CmdProject));
                }
                let w2 = self.width(TINY_STRONG, "New session") + 18.0;
                let chip2 = area(chip.r + 6.0, chip.t, chip.r + 6.0 + w2, chip.b);
                self.round_rect(chip2, 11.0, theme::TAB_FILL)?;
                self.text("New session", TINY_STRONG, chip2, white(0.7), Align::Center)?;
            }
            CmdCard::Task { title, chips } => {
                let tr = button("Add", None, ButtonStyle::Primary, out)?;
                self.outline(x + 6.0, a.t + 21.0, 7.0, 1.5, white(0.5))?;
                let tx = x + 22.0;
                self.text(title, ROW_STRONG, area(tx, a.t + 10.0, tr, a.t + 31.0), theme::LABEL, Align::Leading)?;
                let mut cx = tx;
                for c in chips {
                    let w = self.width(TINY_STRONG, c) + 18.0;
                    let chip = area(cx, a.t + 35.0, cx + w, a.t + 55.0);
                    if chip.r > tr {
                        break;
                    }
                    self.round_rect(chip, 10.0, theme::TAB_FILL)?;
                    self.text(c, TINY_STRONG, chip, theme::LABEL, Align::Center)?;
                    cx = chip.r + 6.0;
                }
            }
        }
        out.hits.push((a, Hit::Cmd(0)));
        Ok(())
    }

    fn paint_panel(&self, left: f32, right: f32, height: f32, p: &Panel) -> Result<Painted> {
        use layout::*;
        if let (Tab::Command, Some(v)) = (p.tab, &p.command) {
            return self.paint_command(left, right, v, p.hover);
        }
        if let (Tab::Deck, Some(e)) = (p.tab, &p.editor) {
            let mut out = Painted::default();
            self.paint_deck_editor(&mut out, left, right, height, e, p.hover)?;
            return Ok(out);
        }
        let mut out = Painted::default();

        // Top row: tab pills on the left; model (Agent) and Settings button on the right.
        let tx = left + 12.0;
        // While the pomodoro runs, the Focus tab label becomes the time left.
        let focus_label = if p.focus.active { p.focus.time.as_str() } else { "Focus" };
        let tabs = [
            (Tab::Today, "Today", Icon::Today),
            (Tab::Agent, "Agent", Icon::Sparkle),
            (Tab::Focus, focus_label, Icon::Timer),
            (Tab::Deck, "Deck", Icon::Keypad),
            (Tab::Widgets, "Widgets", Icon::Widgets),
        ];
        let tabs = tabs.into_iter().filter(|&(t, ..)| t != Tab::Widgets || p.widgets_tab).collect::<Vec<_>>();
        // All tabs in one equal-width segmented track, each with its icon and name,
        // up to the search and Settings buttons.
        let track = area(tx - 2.0, 7.0, right - 10.0 - 26.0 - 28.0 - 6.0, 33.0);
        self.round_rect(track, 13.0, gray(0.067, 1.0))?;
        self.edge(track, 13.0, white(0.05))?;
        let seg_w = (track.r - track.l - 4.0 - (tabs.len() as f32 - 1.0) * 2.0) / tabs.len() as f32;
        for (i, &(tab, label, icon)) in tabs.iter().enumerate() {
            let selected = p.tab == tab;
            let l = track.l + 2.0 + i as f32 * (seg_w + 2.0);
            let seg = area(l, track.t + 2.0, l + seg_w, track.b - 2.0);
            if selected {
                self.round_rect(seg, 11.0, theme::GRAPHITE)?;
            } else if p.hover == Some(Hit::Tab(tab)) {
                self.round_rect(seg, 11.0, with_alpha(theme::GRAPHITE, 0.5))?;
            }
            let color = if selected { theme::LABEL } else { theme::SECONDARY };
            let dot = tab == Tab::Agent && p.agent_working;
            // Icon, name, and the "agent working" dot are centered together within the segment.
            let content = 13.0 + 5.0 + self.width(TAB_SEG, label) + if dot { 7.0 } else { 0.0 };
            let cl = (seg.l + seg.r) / 2.0 - content / 2.0;
            self.icon(icon, cl + 6.5, 20.0, 13.0, 2.0, color)?;
            let lw = self.width(TAB_SEG, label);
            self.text(label, TAB_SEG, area(cl + 18.0, seg.t, cl + 18.0 + lw + 2.0, seg.b), color, Align::Leading)?;
            if dot {
                self.circle(cl + 18.0 + lw + 5.0, 20.0, 2.0, theme::LABEL)?;
            }
            out.hits.push((seg, Hit::Tab(tab)));
        }
        let gear = area(right - 10.0 - 26.0, 7.0, right - 10.0, 33.0);
        let on_settings = p.tab == Tab::Settings;
        if on_settings {
            self.round_rect(gear, 13.0, theme::TAB_FILL)?;
        } else if p.hover == Some(Hit::Tab(Tab::Settings)) {
            self.round_rect(gear, 13.0, with_alpha(theme::TAB_FILL, 0.6))?;
        }
        self.icon(Icon::Sliders, gear.l + 13.0, 20.0, 14.0, 1.9, if on_settings { theme::LABEL } else { theme::SECONDARY })?;
        out.hits.push((gear, Hit::Tab(Tab::Settings)));
        let search = area(gear.l - 28.0, 7.0, gear.l - 2.0, 33.0);
        if p.hover == Some(Hit::Tab(Tab::Command)) {
            self.round_rect(search, 13.0, with_alpha(theme::TAB_FILL, 0.6))?;
        }
        self.icon(Icon::Search, search.l + 13.0, 20.0, 14.0, 1.9, theme::SECONDARY)?;
        out.hits.push((search, Hit::Tab(Tab::Command)));

        let x0 = left + M;
        let r = right - M;
        match p.tab {
            Tab::Today => self.paint_today(&mut out, x0, r, p)?,
            Tab::Agent => self.paint_agents(&mut out, x0, r, height, p)?,
            Tab::Focus => self.paint_focus(&mut out, x0, r, &p.focus, p.hover)?,
            Tab::Widgets => self.paint_widgets(&mut out, x0, r, &p.widgets, p.hover)?,
            Tab::Settings => self.paint_settings(&mut out, x0, r, &p.settings, p.settings_scroll, p.hover)?,
            // Drawn by `paint_command` above; only reached here if its content doesn't exist yet.
            Tab::Command => {}
            Tab::Deck => {
                if let Some(d) = &p.deck {
                    self.paint_deck(&mut out, left, right, d, p.hover)?;
                }
            }
        }
        // Names of the icon-only buttons (search, Settings), when hovered.
        if let Some(Hit::Tab(t)) = p.hover.filter(|_| p.tab != Tab::Command) {
            let name = match t {
                Tab::Settings => Some("Settings"),
                Tab::Command => Some("Search"),
                _ => None,
            };
            let anchor = out.hits.iter().find(|(_, h)| *h == Hit::Tab(t)).map(|(a, _)| *a);
            if let (Some(name), Some(a), false) = (name, anchor, p.tab == t) {
                let w = self.width(TINY_STRONG, name) + 18.0;
                let l = ((a.l + a.r) / 2.0 - w / 2.0).clamp(left + 6.0, right - 6.0 - w);
                let b = area(l, a.b + 3.0, l + w, a.b + 25.0);
                self.round_rect(b, 8.0, gray(0.14, 1.0))?;
                self.edge(b, 8.0, white(0.12))?;
                self.text(name, TINY_STRONG, b, theme::LABEL, Align::Center)?;
            }
        }
        Ok(out)
    }

    /// Today tab: week strip + progress, scrollable task list, and input field at the bottom.
    fn paint_today(&self, out: &mut Painted, x0: f32, r: f32, p: &Panel) -> Result<()> {
        use layout::*;
        // Week strip, flanked by arrows for the previous / next week. The selected day is filled
        // white; today, when not selected, is ringed.
        const COL: f32 = 24.0;
        const NAV: f32 = 16.0;
        let strip_l = x0 - 4.0 + NAV;
        let chevron = |icon: Icon, cx: f32, on: bool, hit: Hit, out: &mut Painted| -> Result<()> {
            let color = if !on {
                white(0.12)
            } else if p.hover == Some(hit) {
                theme::LABEL
            } else {
                white(0.4)
            };
            self.icon(icon, cx, HEAD + 18.0, 12.0, 1.9, color)?;
            out.hits.push((area(cx - NAV / 2.0, HEAD, cx + NAV / 2.0, HEAD + STRIP_H - 2.0), hit));
            Ok(())
        };
        chevron(Icon::ChevronLeft, strip_l - NAV / 2.0, p.week_prev, Hit::WeekPrev, out)?;
        chevron(Icon::ChevronRight, strip_l + 7.0 * COL + NAV / 2.0, p.week_next, Hit::WeekNext, out)?;
        for (i, d) in p.week.iter().enumerate() {
            let cx = strip_l + COL * (i as f32 + 0.5);
            let cell = |t: f32, b: f32| area(cx - COL / 2.0, t, cx + COL / 2.0, b);
            let hit = Hit::Day(i as u8);
            let strong = d.selected || d.today;
            self.text(d.letter, WEEKDAY, cell(HEAD, HEAD + 12.0), if strong { theme::LABEL } else { white(0.3) }, Align::Center)?;
            let cy = HEAD + 24.0;
            let number = cell(cy - 10.0, cy + 10.0);
            if d.selected {
                self.circle(cx, cy, 10.0, theme::LABEL)?;
                self.text(&d.day.to_string(), DAY_TODAY, number, theme::BLACK, Align::Center)?;
            } else {
                if p.hover == Some(hit) {
                    self.circle(cx, cy, 10.0, white(0.1))?;
                }
                if d.today {
                    self.outline(cx, cy, 9.4, 1.2, white(0.55))?;
                    self.text(&d.day.to_string(), DAY_TODAY, number, theme::LABEL, Align::Center)?;
                } else {
                    self.text(&d.day.to_string(), DAY, number, if d.past { white(0.55) } else { white(0.3) }, Align::Center)?;
                }
                if d.busy && !d.today {
                    self.circle(cx, cy + 10.5, 1.4, white(0.5))?;
                }
            }
            out.hits.push((area(cx - COL / 2.0, HEAD, cx + COL / 2.0, HEAD + STRIP_H - 2.0), hit));
        }
        let summary_l = strip_l + 7.0 * COL + NAV + 4.0;
        let (rcx, rcy) = (r - 13.0, HEAD + 18.0);
        if let Some(v) = &p.day {
            // Another day: its date and summary, then a button back to today where the progress ring sits.
            let label = "Today";
            let w = self.width(TINY_STRONG, label) + 20.0;
            let back = area(r + 2.0 - w, rcy - 11.0, r + 2.0, rcy + 11.0);
            let hovered = p.hover == Some(Hit::BackToToday);
            self.round_rect(back, 11.0, if hovered { theme::GRAPHITE } else { theme::TAB_FILL })?;
            self.edge(back, 11.0, white(if hovered { 0.14 } else { 0.08 }))?;
            self.text(label, TINY_STRONG, back, theme::LABEL, Align::Center)?;
            out.hits.push((area(back.l - 4.0, HEAD, r + 6.0, HEAD + STRIP_H - 2.0), Hit::BackToToday));
            let tr = back.l - 10.0;
            self.text(&v.label, SUMMARY, area(summary_l, HEAD + 3.0, tr, HEAD + 19.0), theme::LABEL, Align::Trailing)?;
            self.text(&v.detail, TINY, area(summary_l, HEAD + 18.0, tr, HEAD + 33.0), theme::TERTIARY, Align::Trailing)?;
        } else if p.total == 0 {
            self.text("No tasks", SMALL, area(summary_l, HEAD, r, HEAD + 36.0), theme::TERTIARY, Align::Trailing)?;
        } else {
            let remaining = p.total - p.done;
            self.progress_ring(rcx, rcy, 10.5, 3.0, p.done as f32 / p.total as f32, theme::LABEL)?;
            let tr = rcx - 13.0 - 8.0;
            let headline = if remaining == 0 { "All done".to_string() } else { format!("{remaining} left") };
            let detail = match (p.overdue, p.done) {
                (0, 0) => None,
                (0, d) => Some((format!("{d} done"), theme::TERTIARY)),
                (o, _) => Some((format!("{o} overdue"), theme::AMBER)),
            };
            match detail {
                Some((text, color)) => {
                    self.text(&headline, SUMMARY, area(summary_l, HEAD + 3.0, tr, HEAD + 19.0), theme::LABEL, Align::Trailing)?;
                    self.text(&text, TINY, area(summary_l, HEAD + 18.0, tr, HEAD + 33.0), color, Align::Trailing)?;
                }
                None => self.text(&headline, SUMMARY, area(summary_l, HEAD, tr, HEAD + 36.0), theme::LABEL, Align::Trailing)?,
            }
        }

        // List. Without tasks, Nochi stays in place and the text beside it follows the input field
        // (hint, then preview), so the panel height doesn't jump when input opens.
        let top = HEAD + STRIP_H;
        let previews: &[Preview] = p.input.as_deref().unwrap_or(&[]);
        let notice = p.notice.as_deref().filter(|_| p.input.is_some());
        let list_h = list_height(p.rows.len(), preview_lines(previews.len(), notice.is_some()), p.media.is_some());
        let view = area(x0 - 8.0, top, r + 4.0, top + list_h);
        let mut y = top;
        if p.rows.is_empty() {
            let tile = area(x0, y + 10.0, x0 + 36.0, y + 46.0);
            self.tile(tile, 11.0)?;
            out.pet = Some(PetSpot { center: (x0 + 18.0, y + 28.0), size: pet::Size::Pill });
            let tx = x0 + 48.0;
            if let (Some(v), true) = (&p.day, previews.is_empty()) {
                let (title, hint) = if v.past {
                    ("Nothing logged on this day.", format!("Finished tasks stay here for {} days.", crate::tasks::HISTORY_DAYS))
                } else {
                    ("Nothing planned for this day.", "Tasks due that day show up here.".to_string())
                };
                self.text(title, EMPTY_TITLE, area(tx, y + 9.0, r, y + 27.0), theme::LABEL, Align::Leading)?;
                self.draw_text(&hint, SMALL, area(tx, y + 27.0, r, y + EMPTY_H), theme::SECONDARY, Align::Leading, true)?;
            } else if previews.is_empty() {
                let title = notice.unwrap_or("Nothing on today");
                self.text(title, EMPTY_TITLE, area(tx, y + 9.0, r, y + 27.0), theme::LABEL, Align::Leading)?;
                let hint = if p.input.is_some() {
                    "Separate tasks with commas or \u{201C}then\u{201D}. Enter to save."
                } else {
                    "Add several at once, e.g. \u{201C}review PR, then team meeting at 2\u{201D}."
                };
                self.draw_text(hint, SMALL, area(tx, y + 27.0, r, y + EMPTY_H), theme::SECONDARY, Align::Leading, true)?;
            } else {
                let mut py = y + 5.0;
                for pv in previews.iter().take(MAX_PREVIEW) {
                    self.circle(tx + 3.0, py + 8.5, 2.2, white(0.7))?;
                    self.text(&pv.title, PREVIEW, area(tx + 14.0, py, r - 96.0, py + 17.0), theme::LABEL, Align::Leading)?;
                    if let Some(chip) = &pv.chip {
                        self.text(chip, TINY, area(r - 110.0, py, r, py + 17.0), theme::SECONDARY, Align::Trailing)?;
                    }
                    py += 17.0;
                }
            }
        } else {
            if previews.is_empty() {
                if let Some(n) = notice {
                    self.text(n, SMALL, area(x0, y, r, y + PREVIEW_H), theme::SECONDARY, Align::Leading)?;
                    y += PREVIEW_H;
                }
            }
            for pv in previews.iter().take(MAX_PREVIEW) {
                self.circle(x0 + 7.0, y + PREVIEW_H / 2.0, 2.2, white(0.7))?;
                self.text(&pv.title, PREVIEW, area(x0 + 24.0, y, r - 96.0, y + PREVIEW_H), theme::LABEL, Align::Leading)?;
                if let Some(chip) = &pv.chip {
                    self.text(chip, TINY, area(r - 110.0, y, r, y + PREVIEW_H), theme::SECONDARY, Align::Trailing)?;
                }
                y += PREVIEW_H;
            }
            for row in &p.rows {
                if y + ROW_H > view.b + 0.5 {
                    break;
                }
                let hovered = matches!(p.hover, Some(Hit::Toggle(id)) | Some(Hit::Delete(id)) if id == row.id);
                if hovered {
                    self.round_rect(area(x0 - 6.0, y + 1.0, r + 2.0, y + ROW_H - 1.0), 8.0, white(0.05))?;
                }
                let (cx, cy) = (x0 + 7.0, y + ROW_H / 2.0);
                if row.done {
                    self.circle(cx, cy, 7.0, theme::LABEL)?;
                    self.icon(Icon::Check, cx, cy, 9.0, 1.5, theme::BLACK)?;
                } else {
                    self.outline(cx, cy, 6.3, 1.4, white(0.4))?;
                }
                let title = area(x0 + 24.0, y, r - 72.0, y + ROW_H);
                if row.done {
                    self.struck(&row.title, ROW, title, white(0.3))?;
                } else {
                    self.text(&row.title, ROW, title, theme::LABEL, Align::Leading)?;
                }
                let side = area(r - 22.0, y, r + 4.0, y + ROW_H);
                if hovered {
                    let on_delete = p.hover == Some(Hit::Delete(row.id));
                    self.icon(Icon::Cross, r - 8.0, cy, 11.0, 1.8, if on_delete { theme::LABEL } else { theme::TERTIARY })?;
                } else if let Some(chip) = &row.chip {
                    let (font, color) = if chip.warn { (TINY_STRONG, theme::AMBER) } else { (CHIP, white(0.5)) };
                    self.text(&chip.text, font, area(r - 72.0, y, r, y + ROW_H), color, Align::Trailing)?;
                }
                out.hits.push((side, Hit::Delete(row.id)));
                out.hits.push((area(x0 - 6.0, y, side.l, y + ROW_H), Hit::Toggle(row.id)));
                y += ROW_H;
            }
            if p.more_above > 0 {
                self.fade(area(view.l, view.t, view.r, view.t + 14.0), false)?;
            }
            if p.more_below > 0 {
                self.fade(area(view.l, view.b - 22.0, view.r, view.b), true)?;
            }
            if p.more_above + p.more_below > 0 && p.total > 0 {
                let total = p.total as f32;
                self.scrollbar(view, p.more_above as f32 / total, p.rows.len() as f32 / total)?;
            }
        }
        out.hits.push((view, Hit::List));

        let mut by = top + list_h + BAR_GAP;
        if let Some(m) = &p.media {
            self.paint_media(out, x0, r, top + list_h, m, p.hover)?;
            by += MEDIA_H;
        }

        // Input field, stuck to the bottom.
        let bar = area(x0 - 4.0, by, r + 4.0, by + BAR_H);
        let open = p.input.is_some();
        self.round_rect(bar, BAR_H / 2.0, theme::FIELD)?;
        let edge = D2D1_ROUNDED_RECT { rect: rect(area(bar.l + 0.5, bar.t + 0.5, bar.r - 0.5, bar.b - 0.5)), radiusX: 15.5, radiusY: 15.5 };
        unsafe { self.ctx.DrawRoundedRectangle(&edge, &self.brush(white(if open { 0.14 } else { 0.05 }))?, 1.0, None) };
        let mid = by + BAR_H / 2.0;
        self.icon(Icon::Plus, x0 + 9.0, mid, 13.0, 2.2, white(0.45))?;
        let (send_x, send_r) = (bar.r - 16.0, 12.0);
        self.orb(send_x, mid, send_r, gray(0.227, 1.0), gray(0.05, 1.0))?;
        chrome_ring(self.ctx, self.d2d, send_x, mid, send_r - 0.6, 1.2, 0.9)?;
        self.icon(Icon::ArrowUp, send_x, mid, 11.0, 2.6, gray(0.9, 1.0))?;
        let field = area(x0 + 24.0, by + 6.0, send_x - send_r - 8.0, by + BAR_H - 6.0);
        out.hits.push((area(send_x - send_r - 4.0, by, bar.r, by + BAR_H), Hit::Send));
        if open {
            // The EDIT window is placed right over this area, with the same background color.
            out.input = Some(field);
        } else {
            let color = if p.hover == Some(Hit::Add) { white(0.5) } else { white(0.36) };
            // On another day, this column still adds to today (and reopens it back to today).
            let placeholder = if p.day.is_some() { "Add a task for today\u{2026}" } else { "Add a task\u{2026} (Win+H to dictate)" };
            self.text(placeholder, FIELD, area(field.l, by, field.r, by + BAR_H), color, Align::Leading)?;
            out.hits.push((bar, Hit::Add));
        }
        Ok(())
    }

    /// Prev / play-pause / next buttons end at `right`, centered on `cy`; hit area spans
    /// `top`..`bottom`. Returns the left edge of the first button.
    fn media_buttons(&self, out: &mut Painted, right: f32, cy: f32, top: f32, bottom: f32, c: &MediaControls) -> Result<f32> {
        let toggle = if c.playing { Icon::Pause } else { Icon::Play };
        let buttons = [
            (media::Command::Prev, Icon::Backward, c.can_prev, right - 65.0),
            (media::Command::PlayPause, toggle, true, right - 39.0),
            (media::Command::Next, Icon::Forward, c.can_next, right - 13.0),
        ];
        for (cmd, icon, enabled, cx) in buttons {
            let hovered = enabled && c.hover == Some(Hit::Media(cmd));
            if hovered {
                self.circle(cx, cy, 12.0, white(0.1))?;
            }
            let color = match (enabled, hovered) {
                (false, _) => white(0.2),
                (true, true) => theme::LABEL,
                (true, false) => white(0.7),
            };
            self.icon(icon, cx, cy, 14.0, 1.9, color)?;
            if enabled {
                out.hits.push((area(cx - 13.0, top, cx + 13.0, bottom), Hit::Media(cmd)));
            }
        }
        Ok(right - 78.0)
    }

    /// Single-line text from several parts. If it doesn't fit in `a`, the parts are handed
    /// to the island as a scrolling marquee; one marquee per draw.
    fn line_or_marquee(&self, out: &mut Painted, parts: Vec<(String, Font, D2D1_COLOR_F)>, a: Area) -> Result<()> {
        let width: f32 = parts.iter().map(|(t, f, _)| self.width(*f, t)).sum();
        if width > a.r - a.l && out.marquee.is_none() {
            out.marquee = Some(Marquee { area: a, parts, width });
            return Ok(());
        }
        let mut x = a.l;
        for (text, font, color) in &parts {
            let w = self.width(*font, text);
            self.text(text, *font, area(x, a.t, (x + w + 2.0).min(a.r), a.b), *color, Align::Leading)?;
            x += w;
            if x >= a.r {
                break;
            }
        }
        Ok(())
    }

    /// Single-level media row: equalizer, title and artist, then prev / play-pause / next buttons.
    fn paint_media(&self, out: &mut Painted, x0: f32, r: f32, y: f32, m: &NowPlaying, hover: Option<Hit>) -> Result<()> {
        use layout::MEDIA_H;
        self.line(x0, y + 0.5, r, y + 0.5, theme::HAIRLINE)?;
        let cy = y + MEDIA_H / 2.0 + 1.0;
        // Playing: moving Composition bars (see `Island::set_eq`); paused: dim static icon.
        if m.playing {
            out.eq = Some((x0 + 7.0, cy));
        } else {
            self.icon(Icon::Waveform, x0 + 7.0, cy, 14.0, 1.9, theme::TERTIARY)?;
        }
        let controls = MediaControls { playing: m.playing, can_prev: m.can_prev, can_next: m.can_next, hover };
        let tr = self.media_buttons(out, r + 5.0, cy, y, y + MEDIA_H, &controls)? - 10.0;
        // Title then dim artist; if they don't fit, both scroll.
        let tx = x0 + 24.0;
        let mut parts = vec![(m.title.clone(), ROW, theme::LABEL)];
        if !m.artist.is_empty() {
            parts.push((format!("   {}", m.artist), SMALL, theme::SECONDARY));
        }
        self.line_or_marquee(out, parts, area(tx, y, tr, y + MEDIA_H + 2.0))
    }

    /// Agent tab: sessions as two-level rows with a small Nochi (the working one surrounded by a
    /// spinning ring), scrollable, and expandable to see the last prompt and actions; then the
    /// usage meters.
    fn paint_agents(&self, out: &mut Painted, x0: f32, r: f32, height: f32, p: &Panel) -> Result<()> {
        use layout::*;
        let top = HEAD + 2.0;
        if p.agents.is_empty() {
            let s = &p.agent_setup;
            let (title, hint) = if !s.connect.is_empty() && !s.connected {
                ("Connect an agent", "See its activity, questions and approvals right here.")
            } else {
                ("No agent sessions yet", "Start an agent and its activity shows up here.")
            };
            self.text(title, ROW_STRONG, area(x0, top, r, top + 20.0), theme::LABEL, Align::Leading)?;
            self.text(hint, SMALL, area(x0, top + 20.0, r, top + 38.0), theme::SECONDARY, Align::Leading)?;
            // Each agent's logo as a button, using the same switch as the Settings page.
            let mut by = top + SETUP_TEXT_H;
            let pitch = 68.0;
            for row in s.connect.chunks(SETUP_COLS) {
                for (i, setting) in row.iter().enumerate() {
                    let cx = x0 + 30.0 + i as f32 * pitch;
                    let hit = Hit::Setting(*setting);
                    let hovered = p.hover == Some(hit);
                    self.round_rect(area(cx - 22.0, by, cx + 22.0, by + 44.0), 12.0, if hovered { gray(0.23, 1.0) } else { theme::GRAPHITE })?;
                    let brand = match setting {
                        Setting::Mcp(app) => agent_brand(*app),
                        _ => Brand::Claude,
                    };
                    self.svg(brand_path(brand), cx, by + 22.0, 20.0, theme::LABEL)?;
                    let name = area(cx - pitch / 2.0, by + 48.0, cx + pitch / 2.0, by + 64.0);
                    self.text(connect_label(*setting), TINY, name, if hovered { theme::LABEL } else { theme::SECONDARY }, Align::Center)?;
                    out.hits.push((area(cx - pitch / 2.0 + 2.0, by, cx + pitch / 2.0 - 2.0, by + 64.0), hit));
                }
                by += SETUP_LOGO_H;
            }
            // Quotas exist only for Claude (Claude Code login) and Codex (its session logs).
            let pills = [(s.usage, "Show Claude usage", Setting::ClaudeUsage), (s.codex_usage, "Show Codex usage", Setting::CodexUsage)];
            let mut bx = x0;
            for (_, label, setting) in pills.into_iter().filter(|(on, ..)| *on) {
                let bw = self.width(SMALL, label) + 28.0;
                self.pill_button(out, area(bx, by, bx + bw, by + 26.0), label, false, Hit::Setting(setting), p.hover)?;
                bx += bw + 8.0;
            }
            if s.usage && !s.codex_usage {
                self.text("Reads your Claude Code login", TINY, area(bx + 2.0, by, r, by + 26.0), theme::TERTIARY, Align::Leading)?;
            }
            // The quota stays visible even without sessions.
            return self.paint_meters(x0, r, height, p);
        }
        let meter_rows = meters(p.context, p.usage.as_ref());
        let list = agent_list_height(&p.agents);
        let view = area(x0 - 8.0, top, r + 4.0, top + agent_view_height(list, meter_rows.len()));
        let scroll = p.agent_scroll.clamp(0.0, (list - (view.b - view.t)).max(0.0));
        unsafe { self.ctx.PushAxisAlignedClip(&rect(view), D2D1_ANTIALIAS_MODE_ALIASED) };
        let result = (|| -> Result<()> {
            let mut y = top - scroll;
            for (i, a) in p.agents.iter().enumerate() {
                let h = agent_row_height(a);
                if i > 0 {
                    let ly = y - (AGENT_PITCH - AGENT_ROW_H) / 2.0;
                    self.line(x0, ly, r, ly, theme::HAIRLINE)?;
                }
                let row = area(x0 - 6.0, y, r + 2.0, y + h);
                if row.b > view.t && row.t < view.b {
                    out.hits.push((area(row.l, row.t.max(view.t), row.r, row.b.min(view.b)), Hit::Agent(i)));
                    if a.open || p.hover == Some(Hit::Agent(i)) {
                        self.round_rect(area(row.l, row.t + 1.0, row.r, row.b - 1.0), 9.0, white(if a.open { 0.04 } else { 0.05 }))?;
                    }
                    self.agent_row(out, a, x0, r, y, view)?;
                }
                y += h + (AGENT_PITCH - AGENT_ROW_H);
            }
            Ok(())
        })();
        unsafe { self.ctx.PopAxisAlignedClip() };
        result?;
        let max = (list - (view.b - view.t)).max(0.0);
        if scroll > 0.0 {
            self.fade(area(view.l, view.t, view.r, view.t + 12.0), false)?;
        }
        if scroll < max {
            self.fade(area(view.l, view.b - 20.0, view.r, view.b), true)?;
        }
        if max > 0.0 {
            self.scrollbar(view, scroll / list, (view.b - view.t) / list)?;
        }

        self.paint_meters(x0, r, height, p)
    }

    /// Context and quota meters under the Agent tab.
    fn paint_meters(&self, x0: f32, r: f32, height: f32, p: &Panel) -> Result<()> {
        use layout::*;
        let meters = meters(p.context, p.usage.as_ref());
        if !meters.is_empty() {
            let uy = height - BOTTOM - usage_height(meters.len());
            self.line(x0, uy + 4.0, r, uy + 4.0, theme::HAIRLINE)?;
            // Section title, with the model in use on the right (formerly in the tab bar).
            self.text("USAGE", SECTION, area(x0, uy + 9.0, r, uy + 25.0), theme::TERTIARY, Align::Leading)?;
            if let Some(model) = &p.model {
                self.text(model, TINY, area(x0 + 60.0, uy + 9.0, r, uy + 25.0), theme::SECONDARY, Align::Trailing)?;
            }
            let mut my = uy + USAGE_TOP;
            // Label column as wide as the longest label ("Codex weekly" is wider than "Weekly").
            let label_w = meters.iter().map(|m| self.width(SMALL, m.label)).fold(56.0, f32::max);
            for m in meters {
                let high = m.fraction >= 0.8;
                self.text(m.label, SMALL, area(x0, my, x0 + label_w, my + METER_H), theme::SECONDARY, Align::Leading)?;
                // Time left until reset to the left of the number; the bar shortens to make room.
                let track_r = if m.note.is_some() { r - 100.0 } else { r - 44.0 };
                if let Some(note) = &m.note {
                    self.text(note, SMALL, area(r - 96.0, my, r - 42.0, my + METER_H), theme::TERTIARY, Align::Trailing)?;
                }
                let track = area(x0 + label_w + 8.0, my + 8.5, track_r, my + 11.5);
                self.round_rect(track, 1.5, white(0.1))?;
                let f = m.fraction.clamp(0.0, 1.0);
                if f > 0.0 {
                    self.round_rect(area(track.l, track.t, track.l + (track.r - track.l) * f, track.b), 1.5, if high { theme::AMBER } else { white(m.alpha) })?;
                }
                self.text(&m.value, TINY_STRONG, area(r - 40.0, my, r, my + METER_H), if high { theme::AMBER } else { theme::LABEL }, Align::Trailing)?;
                my += METER_H;
            }
        }
        Ok(())
    }

    /// One session row: Nochi + ring, project name and status, activity; details when expanded.
    fn agent_row(&self, out: &mut Painted, a: &AgentRow, x0: f32, r: f32, y: f32, view: Area) -> Result<()> {
        use layout::*;
        let (cx, cy) = (x0 + 14.0, y + AGENT_ROW_H / 2.0);
        match a.state {
            AgentState::Working => {
                self.orb(cx, cy, 11.0, gray(0.11, 1.0), gray(0.02, 1.0))?;
                // Ring spins only if its whole circle is visible (its sprite isn't
                // clipped by the scroll area); otherwise a static ring.
                let inside = cy - 14.0 >= view.t && cy + 14.0 <= view.b;
                if inside && out.rings.len() < MAX_RINGS {
                    out.rings.push(RingSpot { center: (cx, cy), diameter: 28.0 });
                } else {
                    chrome_ring(self.ctx, self.d2d, cx, cy, 13.3, 1.3, 1.0)?;
                }
            }
            AgentState::Waiting => {
                self.orb(cx, cy, 14.0, gray(0.173, 1.0), gray(0.04, 1.0))?;
                self.outline(cx, cy, 13.4, 1.3, theme::AMBER)?;
            }
            _ => self.orb(cx, cy, 14.0, gray(0.173, 1.0), gray(0.04, 1.0))?,
        }
        self.eyes(cx, cy, a.state)?;

        let x = x0 + 38.0;
        let name_w = self.width(ROW_STRONG, &a.project).min(r - x - 120.0);
        self.text(&a.project, ROW_STRONG, area(x, y + 3.0, x + name_w, y + 21.0), theme::LABEL, Align::Leading)?;
        let (meta, color) = match a.state {
            // Agents via MCP report no steps, only the duration.
            AgentState::Working if a.mcp => (duration(a.minutes_working), white(0.4)),
            AgentState::Working => (format!("{} {} \u{00B7} {}", a.steps, steps_word(a.steps), duration(a.minutes_working)), white(0.4)),
            AgentState::Waiting => ("needs you".to_string(), theme::AMBER),
            AgentState::Done => (format!("done \u{00B7} {}", minutes_ago(a.minutes_ago)), white(0.4)),
            AgentState::Idle => (format!("idle \u{00B7} {}", minutes_ago(a.minutes_ago)), white(0.4)),
        };
        self.text(&meta, TINY, area(x + name_w + 6.0, y + 4.0, r, y + 21.0), color, Align::Leading)?;
        let line = match a.state {
            AgentState::Done if !a.mcp => format!("Finished in {} {}", a.steps, steps_word(a.steps)),
            _ if a.activity == THINKING => match &a.last {
                Some(last) => format!("{THINKING} after {}", lowercase_first(last)),
                None => THINKING.to_string(),
            },
            _ => a.activity.clone(),
        };
        let line_color = match a.state {
            AgentState::Waiting => theme::AMBER,
            AgentState::Done | AgentState::Idle => white(0.4),
            AgentState::Working => white(0.6),
        };
        self.text(&line, AGENT_LINE, area(x, y + 21.0, r, y + 38.0), line_color, Align::Leading)?;

        if a.open {
            let mut dy = y + AGENT_ROW_H + 2.0;
            if let Some(prompt) = &a.prompt {
                self.text(&format!("\u{201C}{prompt}\u{201D}"), SMALL, area(x, dy, r, dy + DETAIL_H), white(0.55), Align::Leading)?;
                dy += DETAIL_H;
            }
            for (i, h) in a.history.iter().enumerate() {
                self.circle(x + 3.0, dy + DETAIL_H / 2.0, 1.6, white(if i == 0 { 0.5 } else { 0.3 }))?;
                self.text(h, TINY, area(x + 12.0, dy, r, dy + DETAIL_H), white(if i == 0 { 0.5 } else { 0.35 }), Align::Leading)?;
                dy += DETAIL_H;
            }
        }
        Ok(())
    }

    /// Focus tab: big progress ring with Nochi inside, time left, task being worked on,
    /// Stop / Start-Pause buttons, then session dots toward the long break.
    fn paint_focus(&self, out: &mut Painted, x0: f32, r: f32, f: &FocusView, hover: Option<Hit>) -> Result<()> {
        use layout::*;
        let (cx, cy, rr) = (x0 + 36.0, HEAD + 46.0, 34.0);
        self.progress_ring(cx, cy, rr, 4.0, f.progress, if f.dim { white(0.5) } else { theme::LABEL })?;
        out.pet = Some(PetSpot { center: (cx, cy), size: pet::Size::Large });

        let on = |cmd| hover == Some(Hit::Focus(cmd));
        let main_x = r - 18.0;
        self.circle(main_x, cy, 18.0, if on(FocusCmd::Toggle) { white(0.86) } else { theme::LABEL })?;
        if f.active && !f.paused {
            self.icon(Icon::Pause, main_x, cy, 15.0, 2.6, theme::BLACK)?;
        } else {
            // Triangle shifted slightly right so it looks centered.
            self.icon(Icon::Play, main_x + 1.0, cy, 15.0, 2.6, theme::BLACK)?;
        }
        out.hits.push((area(main_x - 21.0, cy - 21.0, main_x + 21.0, cy + 21.0), Hit::Focus(FocusCmd::Toggle)));
        let mut text_r = main_x - 18.0 - 12.0;
        if f.active {
            let stop_x = main_x - 44.0;
            self.circle(stop_x, cy, 18.0, if on(FocusCmd::Stop) { gray(0.23, 1.0) } else { theme::GRAPHITE })?;
            self.icon(Icon::Stop, stop_x, cy, 14.0, 2.2, theme::LABEL)?;
            out.hits.push((area(stop_x - 21.0, cy - 21.0, stop_x + 21.0, cy + 21.0), Hit::Focus(FocusCmd::Stop)));
            text_r = stop_x - 18.0 - 12.0;
        }

        let x = cx + rr + 20.0;
        self.text(&f.kicker, SECTION, area(x, cy - 31.0, text_r, cy - 17.0), theme::TERTIARY, Align::Leading)?;
        self.text(&f.time, FOCUS_TIME, area(x, cy - 19.0, text_r, cy + 17.0), theme::LABEL, Align::Leading)?;
        let line = area(x, cy + 15.0, text_r, cy + 31.0);
        let task = match &f.task {
            Some(t) => Some((format!("On: {t} \u{203A}"), theme::SECONDARY)),
            None if f.can_link => Some(("Link a task \u{203A}".to_string(), theme::TERTIARY)),
            None => None,
        };
        if let Some((text, color)) = task {
            self.text(&text, SMALL, line, if on(FocusCmd::Task) { theme::LABEL } else { color }, Align::Leading)?;
            out.hits.push((line, Hit::Focus(FocusCmd::Task)));
        }

        // Session dots: filled ones are completed in this cycle.
        let dy = HEAD + 100.0;
        for i in 0..f.every {
            self.circle(x0 + 4.0 + i as f32 * 12.0, dy, 3.0, if i < f.done { theme::LABEL } else { white(0.16) })?;
        }
        let hint = if f.hotkey { "Ctrl+Alt+P to start or pause" } else { "Long break after the last dot" };
        self.text(hint, TINY, area(x0 + f.every as f32 * 12.0 + 12.0, dy - 8.0, r, dy + 8.0), theme::TERTIARY, Align::Trailing)?;

        // Length of each phase, two columns; a change applies from the next phase.
        let top = dy + 12.0;
        self.line(x0, top, r, top, theme::HAIRLINE)?;
        let mid = (x0 + r) / 2.0;
        let cells = [(Length::Focus, "Focus"), (Length::ShortBreak, "Short break"), (Length::LongBreak, "Long break"), (Length::Every, "Sessions")];
        for (i, (l, title)) in cells.into_iter().enumerate() {
            let (cl, cr) = if i % 2 == 0 { (x0, mid - 10.0) } else { (mid + 10.0, r) };
            let cy = top + 6.0 + LENGTH_ROW_H * (i / 2) as f32 + LENGTH_ROW_H / 2.0;
            self.text(title, SMALL, area(cl, cy - 10.0, cr - 96.0, cy + 10.0), theme::SECONDARY, Align::Leading)?;
            let n = f.lengths.get(l);
            let value = if l == Length::Every { n.to_string() } else { format!("{n} min") };
            let (minus, plus) = (Hit::Setting(Setting::Pomodoro(l, false)), Hit::Setting(Setting::Pomodoro(l, true)));
            let [a, b] = self.stepper(cr, cy, &value, 48.0, hover, minus, plus)?;
            out.hits.push((a, minus));
            out.hits.push((b, plus));
        }
        Ok(())
    }

    /// Widgets tab: each widget in its own card. Prayer card: progress ring around the
    /// moon, the next prayer large beside it with its time left, then five pills; while the adhan
    /// plays this card becomes a small player. Clipboard card: copied text with its kind.
    fn paint_widgets(&self, out: &mut Painted, x0: f32, r: f32, w: &WidgetsView, hover: Option<Hit>) -> Result<()> {
        use layout::*;
        let on = |c| hover == Some(Hit::Widget(c));
        let (cl, cr) = (x0 - 8.0, r + 8.0);
        let (il, ir) = (cl + 12.0, cr - 12.0);
        let card = |t: f32, h: f32| -> Result<()> {
            let a = area(cl, t, cr, t + h);
            self.round_rect(a, 16.0, gray(0.067, 1.0))?;
            self.edge(a, 16.0, white(0.06))
        };
        let link = |out: &mut Painted, text: &str, t: f32, cmd: WidgetCmd| -> Result<()> {
            let tw = self.width(TINY, text);
            let a = area(ir - tw - 2.0, t, ir, t + 16.0);
            self.text(text, TINY, a, if on(cmd) { theme::LABEL } else { white(0.5) }, Align::Trailing)?;
            out.hits.push((area(a.l - 6.0, a.t - 3.0, a.r + 4.0, a.b + 3.0), Hit::Widget(cmd)));
            Ok(())
        };
        let mut y = HEAD + 2.0;

        if w.prayer_on {
            match &w.prayer {
                Some(p) => {
                    card(y, PRAYER_H)?;
                    let ty = y + 10.0;
                    let name = crate::prayer::NAMES[p.next];
                    if let Some(pos) = &p.adhan {
                        // Adhan player: waveform, position, then buttons.
                        for (i, h) in [6.0, 12.0, 8.0, 14.0, 7.0].into_iter().enumerate() {
                            let x = il + i as f32 * 4.5;
                            self.round_rect(area(x, ty + 8.0 - h / 2.0, x + 2.5, ty + 8.0 + h / 2.0), 1.2, theme::AMBER)?;
                        }
                        self.text("Adhan playing", TINY_STRONG, area(il + 30.0, ty, ir - 80.0, ty + 16.0), theme::AMBER, Align::Leading)?;
                        self.text(pos, TINY, area(ir - 90.0, ty, ir, ty + 16.0), white(0.5), Align::Trailing)?;
                        let nw = self.width(WIDGET_BIG, name);
                        self.text(name, WIDGET_BIG, area(il, y + 30.0, il + nw + 2.0, y + 58.0), theme::LABEL, Align::Leading)?;
                        self.text(&format!("now \u{00B7} {}", p.times[p.next]), SMALL, area(il + nw + 9.0, y + 37.0, ir, y + 55.0), theme::SECONDARY, Align::Leading)?;
                        let by = y + 72.0;
                        let sw = self.width(SMALL, "Stop adhan") + 28.0;
                        self.pill_button(out, area(il, by, il + sw, by + 26.0), "Stop adhan", true, Hit::Widget(WidgetCmd::StopAdhan), hover)?;
                        let mw = self.width(SMALL, "Mute today") + 28.0;
                        self.pill_button(out, area(il + sw + 8.0, by, il + sw + 8.0 + mw, by + 26.0), "Mute today", false, Hit::Widget(WidgetCmd::MuteAdhan), hover)?;
                    } else {
                        // Ring around the moon: how far from the previous prayer to the next.
                        let (rx, ry) = (il + 29.0, y + 40.0);
                        self.progress_ring(rx, ry, 26.0, 4.0, p.progress.clamp(0.0, 1.0), theme::AMBER)?;
                        // The moon is drawn by island and rocks gently while this tab is open.
                        out.moon = Some(MoonSpot { center: (rx, ry), size: 22.0, glow: false });
                        // Three rows to its right: city, next prayer and its time, time remaining.
                        let tl = rx + 26.0 + 2.0 + 14.0;
                        self.text(&format!("Next prayer \u{00B7} {}", p.place), SMALL, area(tl, y + 9.0, ir - 52.0, y + 25.0), theme::SECONDARY, Align::Leading)?;
                        link(out, "Change", y + 9.0, WidgetCmd::City)?;
                        self.text(&format!("{name} {}", p.times[p.next]), PRAYER_HERO, area(tl, y + 24.0, ir, y + 56.0), theme::LABEL, Align::Leading)?;
                        let uw = self.width(PRAYER_UNTIL, &p.until);
                        self.text(&p.until, PRAYER_UNTIL, area(tl, y + 55.0, tl + uw + 2.0, y + 72.0), theme::AMBER, Align::Leading)?;
                        self.text(&p.hijri, TINY, area(tl + uw + 10.0, y + 55.0, ir, y + 72.0), theme::TERTIARY, Align::Trailing)?;
                        // Five prayer times as equal-width pills; the next one is highlighted.
                        let (pl, pr, gap) = (il - 4.0, ir + 4.0, 4.0);
                        let pw = (pr - pl - 4.0 * gap) / 5.0;
                        let (pt, pb) = (y + 80.0, y + 116.0);
                        for i in 0..5 {
                            let l = pl + i as f32 * (pw + gap);
                            let cell = area(l, pt, l + pw, pb);
                            let (past, next) = (i < p.passed && i != p.next, i == p.next);
                            if next {
                                self.round_rect(cell, 10.0, gray(0.114, 1.0))?;
                                self.edge(cell, 10.0, white(0.07))?;
                            }
                            let (nc, tc) = if next { (theme::AMBER, theme::LABEL) } else if past { (white(0.25), white(0.42)) } else { (white(0.5), white(0.85)) };
                            self.text(crate::prayer::NAMES[i], PRAYER_PILL_NAME, area(cell.l, pt + 4.0, cell.r, pt + 18.0), nc, Align::Center)?;
                            self.text(&p.times[i], PRAYER_PILL_TIME, area(cell.l, pt + 17.0, cell.r, pt + 33.0), tc, Align::Center)?;
                        }
                    }
                    y += PRAYER_H;
                }
                None => {
                    card(y, PRAYER_EMPTY_H)?;
                    let ty = y + 10.0;
                    self.icon(Icon::Moon, il + 6.0, ty + 8.0, 12.0, 1.8, theme::SECONDARY)?;
                    self.text("Prayer times", TINY, area(il + 18.0, ty, ir, ty + 16.0), theme::SECONDARY, Align::Leading)?;
                    let hint = match (&w.location_error, w.locating) {
                        (_, true) => "Finding your location\u{2026}",
                        (Some(e), _) => e.as_str(),
                        _ => "Pick your city to see today's times, worked out on this PC.",
                    };
                    let warn = w.location_error.is_some() && !w.locating;
                    self.text(hint, SMALL, area(il, y + 30.0, ir, y + 48.0), if warn { theme::AMBER } else { theme::LABEL }, Align::Leading)?;
                    let by = y + 56.0;
                    let sw = self.width(SMALL, "Search city") + 28.0;
                    self.pill_button(out, area(il, by, il + sw, by + 26.0), "Search city", true, Hit::Widget(WidgetCmd::City), hover)?;
                    let lw = self.width(SMALL, "Use my location") + 28.0;
                    self.pill_button(out, area(il + sw + 8.0, by, il + sw + 8.0 + lw, by + 26.0), "Use my location", false, Hit::Widget(WidgetCmd::Locate), hover)?;
                    y += PRAYER_EMPTY_H;
                }
            }
        }

        let Some(clips) = &w.clips else { return Ok(()) };
        if w.prayer_on {
            y += WIDGET_GAP;
        }
        card(y, CLIP_HEAD + clips.len().max(1) as f32 * CLIP_ROW_H + 6.0)?;
        let ty = y + 10.0;
        self.icon(Icon::Clipboard, il + 6.0, ty + 8.0, 12.0, 1.8, theme::SECONDARY)?;
        self.text("Clipboard", TINY, area(il + 18.0, ty, ir - 56.0, ty + 16.0), theme::SECONDARY, Align::Leading)?;
        if !clips.is_empty() {
            link(out, "Clear", ty, WidgetCmd::ClearClips)?;
        }
        let mut ry = y + CLIP_HEAD;
        if clips.is_empty() {
            return self.text("Text you copy shows up here. Kept in memory only.", TINY, area(il, ry, ir, ry + CLIP_ROW_H), theme::TERTIARY, Align::Leading);
        }
        for (i, c) in clips.iter().enumerate() {
            let row = area(il - 6.0, ry, ir + 6.0, ry + CLIP_ROW_H);
            let hovered = on(WidgetCmd::Clip(i));
            if hovered {
                self.round_rect(area(row.l, row.t + 1.0, row.r, row.b - 1.0), 8.0, white(0.06))?;
            }
            let cy = ry + CLIP_ROW_H / 2.0;
            let tile = area(il, cy - 10.0, il + 20.0, cy + 10.0);
            self.round_rect(tile, 6.0, gray(0.115, 1.0))?;
            self.edge(tile, 6.0, white(0.08))?;
            let glyph = match c.kind {
                ClipKind::Command => Icon::Terminal,
                ClipKind::Link => Icon::Link,
                ClipKind::Text => Icon::Lines,
            };
            self.icon(glyph, il + 10.0, cy, 11.0, 2.0, theme::LABEL)?;
            // Single line: whitespace and newlines are collapsed.
            let line: String = c.text.split_whitespace().collect::<Vec<_>>().join(" ");
            let font = if c.kind == ClipKind::Command { MONO_SMALL } else { SMALL };
            let text_r = if hovered { ir - 64.0 } else { ir - 34.0 };
            self.text(&line, font, area(il + 29.0, ry, text_r, ry + CLIP_ROW_H), if hovered { theme::LABEL } else { white(0.85) }, Align::Leading)?;
            if hovered {
                let b = area(ir - 58.0, cy - 11.0, ir, cy + 11.0);
                self.round_rect(b, 11.0, theme::LABEL)?;
                self.icon(Icon::Copy, b.l + 14.0, cy, 11.0, 2.2, theme::BLACK)?;
                self.text("Copy", TINY_STRONG, area(b.l + 22.0, b.t, b.r - 6.0, b.b), theme::BLACK, Align::Center)?;
            } else {
                self.text(&c.age, TINY, area(ir - 32.0, ry, ir, ry + CLIP_ROW_H), white(0.4), Align::Trailing)?;
            }
            out.hits.push((row, Hit::Widget(WidgetCmd::Clip(i))));
            ry += CLIP_ROW_H;
        }
        Ok(())
    }

    /// Tooltip bubble below `anchor` (above it if it doesn't fit), inside `bounds`.
    fn tooltip(&self, anchor: Area, title: &str, desc: &str, bounds: Area) -> Result<()> {
        const PAD: f32 = 11.0;
        let w = 280.0_f32.min(bounds.r - bounds.l);
        let text_w = w - 2.0 * PAD;
        let h = 8.0 + 16.0 + text_height(self.dwrite, TINY, desc, text_w) + 9.0;
        let l = anchor.l.clamp(bounds.l, bounds.r - w);
        let t = if anchor.b + 4.0 + h <= bounds.b { anchor.b + 4.0 } else { (anchor.t - 4.0 - h).max(bounds.t) };
        let b = area(l, t, l + w, t + h);
        self.round_rect(b, 10.0, gray(0.14, 1.0))?;
        self.edge(b, 10.0, white(0.12))?;
        self.text(title, TINY_STRONG, area(b.l + PAD, b.t + 7.0, b.r - PAD, b.t + 23.0), theme::LABEL, Align::Leading)?;
        self.draw_text(desc, TINY, area(b.l + PAD, b.t + 23.0, b.r - PAD, b.b - 4.0), theme::SECONDARY, Align::Leading, true)
    }

    /// − / + buttons with the value (`value_w` wide) between them; returns the area of both buttons.
    #[allow(clippy::too_many_arguments)]
    fn stepper(&self, r: f32, cy: f32, value: &str, value_w: f32, hover: Option<Hit>, minus: Hit, plus: Hit) -> Result<[Area; 2]> {
        const B: f32 = 11.0;
        let plus_x = r - B;
        let minus_x = plus_x - 2.0 * B - value_w;
        for (x, hit, icon) in [(minus_x, minus, Icon::Minus), (plus_x, plus, Icon::Plus)] {
            self.circle(x, cy, B, if hover == Some(hit) { gray(0.23, 1.0) } else { theme::GRAPHITE })?;
            self.icon(icon, x, cy, 11.0, 2.0, theme::LABEL)?;
        }
        self.text(value, SMALL, area(minus_x + B, cy - 10.0, plus_x - B, cy + 10.0), theme::LABEL, Align::Center)?;
        Ok([minus_x, plus_x].map(|x| area(x - B - 3.0, cy - B - 3.0, x + B + 3.0, cy + B + 3.0)))
    }

    /// iOS-style switch in monochrome: on = white track with a black knob.
    fn switch(&self, r: f32, cy: f32, on: bool, enabled: bool) -> Result<()> {
        let a = area(r - 30.0, cy - 9.0, r, cy + 9.0);
        let dim = if enabled { 1.0 } else { 0.35 };
        self.round_rect(a, 9.0, if on { white(dim) } else { with_alpha(theme::GRAPHITE, dim) })?;
        let kx = if on { a.r - 9.0 } else { a.l + 9.0 };
        self.circle(kx, cy, 7.0, if on { gray(0.0, dim) } else { white(0.9 * dim) })
    }

    /// Two-option segmented control; returns the area of each segment.
    fn segmented<const N: usize>(&self, r: f32, cy: f32, labels: [&str; N], selected: usize) -> Result<[Area; N]> {
        // All segments as wide as the longest label, so nothing gets clipped.
        let seg_w = labels.iter().map(|l| self.width(TINY, l) + 18.0).fold(46.0_f32, f32::max);
        let track = area(r - N as f32 * seg_w - 4.0, cy - 11.0, r, cy + 11.0);
        self.round_rect(track, 11.0, theme::FIELD)?;
        let segs: [Area; N] = std::array::from_fn(|i| area(track.l + 2.0 + i as f32 * seg_w, track.t + 2.0, track.l + 2.0 + (i + 1) as f32 * seg_w, track.b - 2.0));
        for (i, (seg, label)) in segs.iter().zip(labels).enumerate() {
            if i == selected {
                self.round_rect(*seg, 9.0, theme::GRAPHITE)?;
            }
            self.text(label, TINY, *seg, if i == selected { theme::LABEL } else { theme::SECONDARY }, Align::Center)?;
        }
        Ok(segs)
    }

    /// Settings page: one scrollable list (Notch, Agents, Connectors).
    fn paint_settings(&self, out: &mut Painted, x0: f32, r: f32, v: &SettingsView, scroll: f32, hover: Option<Hit>) -> Result<()> {
        use layout::*;
        let view = area(x0 - 8.0, HEAD, r + 8.0, HEAD + SETTINGS_VIEW_H);
        let scroll = scroll.clamp(0.0, settings_max_scroll());
        unsafe { self.ctx.PushAxisAlignedClip(&rect(view), D2D1_ANTIALIAS_MODE_ALIASED) };
        let result = (|| -> Result<()> {
            let mut y = HEAD - scroll;
            let visible = |t: f32, b: f32| b > view.t && t < view.b;
            let hit = |out: &mut Painted, t: f32, b: f32, a: Area, s: Setting| {
                if visible(t, b) {
                    let a = area(a.l, a.t.max(view.t), a.r, a.b.min(view.b));
                    out.hits.push((a, Hit::Setting(s)));
                }
            };
            let hover_bg = |y: f32, h: f32, s: Setting| -> Result<()> {
                if hover == Some(Hit::Setting(s)) {
                    self.round_rect(area(x0 - 6.0, y + 2.0, r + 6.0, y + h - 2.0), 9.0, white(0.05))?;
                }
                Ok(())
            };
            let section = |y: f32, title: &str| self.text(title, SECTION, area(x0, y + 4.0, r, y + SECTION_H - 2.0), theme::TERTIARY, Align::Leading);
            let label = |x: f32, y: f32, h: f32, title: &str, detail: Option<&str>, dim: bool| -> Result<()> {
                let color = if dim { white(0.6) } else { theme::LABEL };
                match detail {
                    Some(d) => {
                        self.text(title, ROW, area(x, y + 3.0, r - 100.0, y + 21.0), color, Align::Leading)?;
                        self.text(d, TINY, area(x, y + 20.0, r - 60.0, y + h - 4.0), theme::TERTIARY, Align::Leading)
                    }
                    None => self.text(title, ROW, area(x, y, r - 60.0, y + h), color, Align::Leading),
                }
            };

            section(y, "NOTCH")?;
            y += SECTION_H;
            hover_bg(y, SETTING_ROW_TALL, Setting::Shy)?;
            label(x0, y, SETTING_ROW_TALL, "Peek under windows", Some("Only Nochi shows over maximized windows"), false)?;
            self.switch(r, y + SETTING_ROW_TALL / 2.0, v.shy, true)?;
            hit(out, y, y + SETTING_ROW_TALL, area(x0 - 6.0, y, r + 6.0, y + SETTING_ROW_TALL), Setting::Shy);
            y += SETTING_ROW_TALL;
            for (setting, title, on) in [
                (Setting::HideFullscreen, "Hide in fullscreen", v.hide_fullscreen),
                (Setting::Chips, "Smart chips", v.chips),
                (Setting::Autostart, "Start with Windows", v.autostart),
                (Setting::Sound, "Sounds", v.sound),
            ] {
                hover_bg(y, SETTING_ROW_H, setting)?;
                label(x0, y, SETTING_ROW_H, title, None, false)?;
                self.switch(r, y + SETTING_ROW_H / 2.0, on, true)?;
                hit(out, y, y + SETTING_ROW_H, area(x0 - 6.0, y, r + 6.0, y + SETTING_ROW_H), setting);
                y += SETTING_ROW_H;
            }
            label(x0, y, SETTING_ROW_H, "Display", None, false)?;
            let segs = self.segmented(r, y + SETTING_ROW_H / 2.0, ["Stay", "Follow app"], v.follow_app as usize)?;
            hit(out, y, y + SETTING_ROW_H, segs[0], Setting::FollowApp(false));
            hit(out, y, y + SETTING_ROW_H, segs[1], Setting::FollowApp(true));
            y += SETTING_ROW_H;
            label(x0, y, SETTING_ROW_H, "Notch size", None, false)?;
            let selected = match v.size {
                NotchSize::Shy => 0,
                NotchSize::Compact => 1,
                NotchSize::Full => 2,
            };
            let segs = self.segmented(r, y + SETTING_ROW_H / 2.0, ["Shy", "Compact", "Full"], selected)?;
            for (seg, size) in segs.iter().zip([NotchSize::Shy, NotchSize::Compact, NotchSize::Full]) {
                hit(out, y, y + SETTING_ROW_H, *seg, Setting::Size(size));
            }
            y += SETTING_ROW_H + SECTION_GAP;

            section(y, "WIDGETS")?;
            y += SECTION_H;
            // Widget as a Deck-style button: click to turn on or off (amber = on).
            let cell_w = (r - x0) / layout::DECK_COLS as f32;
            for (i, (setting, title, glyph, on)) in
                [(Setting::PrayerWidget, "Prayer times", Icon::Moon, v.prayer_widget), (Setting::ClipHistory, "Clipboard", Icon::Copy, v.clip_history)].into_iter().enumerate()
            {
                let cx = x0 + i as f32 * cell_w;
                let cell = area(cx + 1.0, y, cx + cell_w - 1.0, y + DECK_KEY_H - 2.0);
                let key = DeckKey { label: title.to_string(), glyph, image: None, number: None, on };
                self.deck_key(out, cell, &key, Hit::Setting(setting), hover)?;
                if !on {
                    // Off: the button is dimmed so it stands apart from the lit ones at a glance.
                    let mid = (cell.l + cell.r) / 2.0;
                    self.round_rect(area(mid - 21.0, cell.t + 4.0, mid + 21.0, cell.t + 46.0), 13.0, gray(0.0, 0.5))?;
                }
                // Click area follows the Settings page scroll, not the one set by `deck_key`.
                out.hits.pop();
                hit(out, y, y + DECK_KEY_H, cell, setting);
            }
            y += DECK_KEY_H + 4.0;
            // Prayer reminders only matter while the prayer-times widget is on.
            let h = SETTING_ROW_TALL;
            if v.prayer_widget {
                hover_bg(y, h, Setting::PrayerRemind)?;
                hit(out, y, y + h, area(x0 - 6.0, y, r + 6.0, y + h), Setting::PrayerRemind);
            }
            label(x0, y, h, "Prayer reminders", Some("A glance 10 min before, and a sound at the time"), !v.prayer_widget)?;
            self.switch(r, y + h / 2.0, v.prayer_remind, v.prayer_widget)?;
            y += h;
            // Sound at the time: plain or full adhan (needs Sounds on).
            let enabled = v.prayer_widget && v.prayer_remind && v.sound;
            label(x0, y, SETTING_ROW_H, "Prayer sound", None, !enabled)?;
            let segs = self.segmented(r, y + SETTING_ROW_H / 2.0, ["Chime", "Adhan"], v.adhan as usize)?;
            if enabled {
                hit(out, y, y + SETTING_ROW_H, segs[0], Setting::Adhan(false));
                hit(out, y, y + SETTING_ROW_H, segs[1], Setting::Adhan(true));
            }
            y += SETTING_ROW_H + SECTION_GAP;

            section(y, "AGENTS")?;
            y += SECTION_H;
            if v.claude.is_some() {
                hover_bg(y, SETTING_ROW_H, Setting::Claude)?;
                hit(out, y, y + SETTING_ROW_H, area(x0 - 6.0, y, r + 6.0, y + SETTING_ROW_H), Setting::Claude);
            }
            self.brand(Brand::Claude, x0 + 11.0, y + SETTING_ROW_H / 2.0, 1.0)?;
            let claude = if v.claude.is_none() { "Claude Code (notch-bridge.exe not found)" } else { "Claude Code" };
            label(x0 + 32.0, y, SETTING_ROW_H, claude, None, v.claude.is_none())?;
            self.switch(r, y + SETTING_ROW_H / 2.0, v.claude == Some(true), v.claude.is_some())?;
            y += SETTING_ROW_H;
            let h = SETTING_ROW_TALL;
            hover_bg(y, h, Setting::ClaudeUsage)?;
            hit(out, y, y + h, area(x0 - 6.0, y, r + 6.0, y + h), Setting::ClaudeUsage);
            self.brand(Brand::Claude, x0 + 11.0, y + h / 2.0, 1.0)?;
            label(x0 + 32.0, y, h, "Claude usage", Some("Reads your Claude Code login"), false)?;
            self.switch(r, y + h / 2.0, v.usage, true)?;
            y += h;
            // Other apps via the notch MCP server; those not installed are dimmed.
            for (app, state) in McpApp::ALL.into_iter().zip(v.mcp) {
                let s = Setting::Mcp(app);
                if state.is_some() {
                    hover_bg(y, SETTING_ROW_H, s)?;
                    hit(out, y, y + SETTING_ROW_H, area(x0 - 6.0, y, r + 6.0, y + SETTING_ROW_H), s);
                }
                self.agent_logo(app, x0 + 11.0, y + SETTING_ROW_H / 2.0, if state.is_some() { 1.0 } else { 0.5 })?;
                let title = if state.is_none() { format!("{} (not installed)", app.name()) } else { app.name().to_string() };
                label(x0 + 32.0, y, SETTING_ROW_H, &title, None, state.is_none())?;
                self.switch(r, y + SETTING_ROW_H / 2.0, state == Some(true), state.is_some())?;
                y += SETTING_ROW_H;
                if app == McpApp::Codex {
                    // The Codex quota doesn't need MCP: it's read from session logs, so Codex just has to be installed.
                    let h = SETTING_ROW_TALL;
                    if state.is_some() {
                        hover_bg(y, h, Setting::CodexUsage)?;
                        hit(out, y, y + h, area(x0 - 6.0, y, r + 6.0, y + h), Setting::CodexUsage);
                    }
                    self.agent_logo(app, x0 + 11.0, y + h / 2.0, if state.is_some() { 1.0 } else { 0.5 })?;
                    label(x0 + 32.0, y, h, "Codex usage", Some("Reads Codex's session logs"), state.is_none())?;
                    self.switch(r, y + h / 2.0, v.codex_usage, state.is_some())?;
                    y += h;
                }
            }
            // The rest: copy the MCP server entry to paste into any app that supports MCP.
            hover_bg(y, SETTING_ROW_H, Setting::CopyMcp)?;
            self.tile(area(x0, y + SETTING_ROW_H / 2.0 - 11.0, x0 + 22.0, y + SETTING_ROW_H / 2.0 + 11.0), 7.0)?;
            self.icon(Icon::Plus, x0 + 11.0, y + SETTING_ROW_H / 2.0, 13.0, 1.6, theme::LABEL)?;
            label(x0 + 32.0, y, SETTING_ROW_H, "Other MCP apps", None, false)?;
            let bw = self.width(SMALL, "Copy setup") + 24.0;
            let b = area(r - bw + 6.0, y + 6.0, r + 6.0, y + SETTING_ROW_H - 6.0);
            let copy = if v.mcp_copied { "Copied" } else { "Copy setup" };
            self.pill_button(out, b, copy, false, Hit::Setting(Setting::CopyMcp), hover)?;
            out.hits.pop();
            hit(out, y, y + SETTING_ROW_H, area(x0 - 6.0, y, r + 6.0, y + SETTING_ROW_H), Setting::CopyMcp);
            Ok(())
        })();
        unsafe { self.ctx.PopAxisAlignedClip() };
        result?;

        // Tooltip for the hovered setting, below (or above) the area it points to.
        let hovered = out.hits.iter().rev().find_map(|&(a, h)| match h {
            Hit::Setting(s) if hover == Some(h) => Some((a, s)),
            _ => None,
        });
        if let Some((title, desc, anchor)) = hovered.and_then(|(a, s)| setting_tip(s).map(|(t, d)| (t, d, a))) {
            self.tooltip(anchor, title, desc, area(x0 - 6.0, view.t, r + 6.0, view.b))?;
        }

        let max = settings_max_scroll();
        if scroll > 0.0 {
            self.fade(area(view.l, view.t, view.r, view.t + 14.0), false)?;
        }
        if scroll < max {
            self.fade(area(view.l, view.b - 26.0, view.r, view.b), true)?;
        }
        if max > 0.0 {
            self.scrollbar(area(view.l, view.t, r + 6.0, view.b), scroll / SETTINGS_CONTENT_H, SETTINGS_VIEW_H / SETTINGS_CONTENT_H)?;
        }
        Ok(())
    }
}

fn lowercase_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::layout::*;
    use super::*;

    #[test]
    fn panel_heights_stay_within_canvas() {
        use crate::geometry::{CANVAS_H, PANEL_MIN_H};
        assert_eq!(today_height(0, None, false), HEAD + STRIP_H + EMPTY_H + BAR_GAP + BAR_H + BOTTOM);
        for media in [false, true] {
            assert!(today_height(99, Some(99), media) <= CANVAS_H);
            assert!(today_height(task_rows_fit(None, media), None, media) <= CANVAS_H);
            assert!(today_height(task_rows_fit(Some(1), media), Some(1), media) <= CANVAS_H);
        }
        assert!(today_height(1, Some(1), false) > today_height(1, None, false));
        assert_eq!(today_height(0, None, true), today_height(0, None, false) + MEDIA_H);
        // Without tasks, the height stays the same when input opens or the preview appears.
        assert_eq!(list_height(0, None, false), EMPTY_H);
        assert_eq!(list_height(0, Some(3), true), EMPTY_H);
        // Merely opening the input field adds no row.
        assert_eq!(preview_lines(0, false), None);
        assert_eq!(preview_lines(0, true), Some(1));
        assert_eq!(preview_lines(5, false), Some(MAX_PREVIEW));
        assert!(agent_height(9999.0, 3) <= CANVAS_H);
        assert!(agent_height(agent_list_height(&[]), 0) <= PANEL_MIN_H, "an empty list fits in the minimum height");
        assert_eq!(settings_height(), CANVAS_H);
        assert!((PANEL_MIN_H..=CANVAS_H).contains(&focus_height()));
        assert!(settings_max_scroll() > 0.0);
    }

    #[test]
    fn preview_rows_take_room_from_tasks() {
        assert_eq!(task_rows_fit(None, false), MAX_ROWS);
        assert_eq!(task_rows_fit(Some(1), false), 4);
        assert_eq!(task_rows_fit(Some(3), false), 2);
        // The media row takes one task row.
        assert_eq!(task_rows_fit(None, true), MAX_ROWS - 1);
        assert_eq!(task_rows_fit(Some(3), true), 1);
    }

    #[test]
    fn cards_grow_with_their_text() {
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.unwrap();
        // Single-line command: the permission card keeps its original height; long commands wrap and the card grows taller.
        assert_eq!(permission_body(&dwrite, false, "npm test").natural(), 104.0);
        let long = "cargo build --release --target x86_64-pc-windows-gnu ".repeat(4);
        assert!(permission_body(&dwrite, false, &long).natural() > 140.0);

        let opt = |l: &str, d: Option<&str>| (l.to_string(), d.map(str::to_string), false);
        let short = [opt("Live", Some("Run codex")), opt("Skip", None)];
        let q = question_body(&dwrite, "Which test?", &short, false);
        assert!(q.rows.iter().all(|&(h, stacked)| h == OPTION_H && stacked.is_none()), "label dan keterangan pendek tetap satu baris");
        let detail = "Rebuild every crate in release mode, run the full test suite, then install the binaries into the Programs folder";
        let long = [opt("Full", Some(detail)), opt("Skip", None)];
        let l = question_body(&dwrite, "Which test?", &long, false);
        assert!(l.rows[0].1.is_some() && l.rows[0].0 > OPTION_H + 20.0,"keterangan panjang turun ke bawah label dan dibungkus utuh");
        assert!(l.natural() > q.natural());
        // Multi-select adds a Done row.
        assert_eq!(question_body(&dwrite, "Which test?", &short, true).h, q.h + OPTION_H + OPTION_GAP);
    }

    #[test]
    fn opened_agent_rows_grow_and_the_list_scrolls() {
        let row = |open: bool| AgentRow {
            project: "notch".into(),
            prompt: Some("Add a music widget".into()),
            history: vec!["Editing a.rs".into(), "Reading b.rs".into()],
            open,
            mcp: false,
            activity: THINKING.into(),
            last: None,
            state: AgentState::Working,
            steps: 3,
            minutes_ago: 0,
            minutes_working: 2,
        };
        assert_eq!(agent_row_height(&row(false)), AGENT_ROW_H);
        assert_eq!(agent_row_height(&row(true)), AGENT_ROW_H + 4.0 + 3.0 * DETAIL_H);
        let many: Vec<AgentRow> = (0..8).map(|_| row(false)).collect();
        let list = agent_list_height(&many);
        assert!(agent_view_height(list, 1) < list, "eight sessions do not fit at once");
        assert!(agent_height(list, 1) <= crate::geometry::CANVAS_H);
    }

    #[test]
    fn meters_show_only_what_is_known() {
        assert!(meters(None, None).is_empty());
        // Desktop app: only context from the transcript.
        let m = meters(Some(608_802), None);
        assert_eq!((m.len(), m[0].value.as_str()), (1, "608k"));
        assert!((m[0].fraction - 0.6088).abs() < 1e-3, "di atas 200k dianggap jendela 1M");
        assert!((meters(Some(50_000), None)[0].fraction - 0.25).abs() < 1e-6);
        // Terminal: statusLine quota is shown too; transcript context takes precedence.
        let u = Usage { context_pct: Some(9.0), five_hour_pct: Some(42.0), ..Default::default() };
        let m = meters(Some(20_000), Some(&u));
        assert_eq!(m.iter().map(|m| m.label).collect::<Vec<_>>(), ["Context", "5-hour"]);
        assert_eq!(meters(None, Some(&u))[0].value, "9%");
        assert_eq!(tokens_label(1_250_000), "1.2M");
    }

    #[test]
    fn glance_text_splits_title_and_detail() {
        assert_eq!(glance_parts("Claude finished · notch · 3 steps"), ("Claude finished".into(), Some("notch · 3 steps".into())));
        assert_eq!(glance_parts("2 tasks added"), ("2 tasks added".into(), None));
    }

    #[test]
    fn relative_times() {
        assert_eq!(minutes_ago(0), "just now");
        assert_eq!(minutes_ago(12), "12 min ago");
        assert_eq!(minutes_ago(130), "2 h ago");
        assert_eq!(duration(0), "<1 min");
        assert_eq!(duration(75), "1 h 15 min");
        assert_eq!(lowercase_first("Writing a.py"), "writing a.py");
    }
}

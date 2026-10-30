//! Control center: holds state, receives every event (mouse, tray, hotkey, WinEvent, timer, input),
//! and decides what the notch shows via `state::decide`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use std::collections::BTreeMap;

use notch_protocol::{Choice, Decision, HookEvent, Mcp, McpCall, Message, Question, Reply, MCP_WAIT_SECS};

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Graphics::Gdi::HMONITOR,
    Win32::System::DataExchange::COPYDATASTRUCT,
    Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RETURN,
        VK_RWIN, VK_SHIFT, VK_TAB, VK_UP,
    },
    Win32::UI::WindowsAndMessaging::*,
};

use crate::agents::{self, steps_word, AgentState, Agents, Notice};
use crate::chips::{self, Clip, Thumb};
use crate::clock::Date;
use crate::codex_quota;
use crate::command::{self, Entry, Intent, Kind};
use crate::deck::{self, Button, Deck, Glyph};
use crate::dragdrop::{self, Dropped};
use crate::geometry::{self, shape_for, Placement, Rect, Shape};
use crate::input::{self, Input};
use crate::media::{self, NowPlaying};
use crate::island::{self, Island};
use crate::launcher::{self, Index};
use crate::paint::{
    self, glance_parts, layout, AgentRow, AgentSetup, ButtonStyle, Chip, ChipButton, ChipView, CmdCard, CmdRow, CommandView, Content, DeckEditor, DeckGrid, DeckKey, DeckPage, EditCmd,
    EditField, FocusCmd, FocusView, Hit, Icon, Lead, MediaControls, Painted,
    ClipKind, ClipRow, DayView, Panel, PrayerView, Preview, Row, Setting, SettingsView, Tab, WeekDay, WidgetCmd, WidgetsView,
};
use crate::parse::{parse, Parsed};
use crate::pet::Mood;
use crate::pipe::{self, Incoming};
use crate::pomodoro::{self, Finished, Phase, Pomodoro, Wake};
use crate::quota;
use crate::chime::{self, Chime};
use crate::{adhan, location, prayer};
use crate::settings::{MonitorMode, NotchSize, Settings, MAX_PROJECTS};
use crate::spring::Spring;
use crate::state::{decide, Env, Visual};
use crate::tasks::{self, Store, Task};
use crate::tray::{MenuCmd, Tray};
use crate::watch::Watcher;
use crate::agent_setup::McpApp;
use crate::{agent_setup, autostart, clock, display, tray, watch};

pub const WM_APP_TRAY: u32 = WM_APP + 1;
pub const WM_APP_WINEVENT: u32 = WM_APP + 2;
pub const WM_APP_CLICK: u32 = WM_APP + 3;
pub const WM_APP_HOVER_ENTER: u32 = WM_APP + 4;
pub const WM_APP_HOVER_LEAVE: u32 = WM_APP + 5;
pub const WM_APP_OUTSIDE_CLICK: u32 = WM_APP + 6;
pub const WM_APP_ESCAPE: u32 = WM_APP + 7;
pub const WM_APP_RELAYOUT: u32 = WM_APP + 8;
pub const WM_APP_MOUSEMOVE: u32 = WM_APP + 9;
pub const WM_APP_WHEEL: u32 = WM_APP + 10;
pub const WM_APP_INPUT_CHANGED: u32 = WM_APP + 11;
pub const WM_APP_INPUT_SUBMIT: u32 = WM_APP + 12;
pub const WM_APP_AGENT: u32 = WM_APP + 13;
pub const WM_APP_MEDIA: u32 = WM_APP + 14;
/// Download finished; LPARAM = `Box<String>` holding the file path.
pub const WM_APP_DOWNLOAD: u32 = WM_APP + 15;
/// Arrow or Tab key in the input field; WPARAM = key code.
pub const WM_APP_INPUT_KEY: u32 = WM_APP + 16;
/// Right-click on the notch; LPARAM as in WM_APP_CLICK.
pub const WM_APP_RCLICK: u32 = WM_APP + 17;
/// Combination recorded for a deck button; WPARAM = main key, LPARAM = modifiers (bit 1 Ctrl, 2 Shift, 4 Alt, 8 Win).
const WM_APP_DECK_KEY: u32 = WM_APP + 18;
/// App, file, or link dragged onto the notch (see `dragdrop`).
pub const WM_APP_DRAG: u32 = WM_APP + 19;
/// Notch dragged to another monitor; WPARAM `island::MOVE_*`, LPARAM cursor position.
pub const WM_APP_MOVE: u32 = WM_APP + 20;
/// Official Claude quota has been fetched (see `quota`).
pub const WM_APP_QUOTA: u32 = WM_APP + 21;
/// Position from Windows Location is already available (see `location`).
pub const WM_APP_LOCATION: u32 = WM_APP + 22;

const T_SETTLE: usize = 1;
const T_GLANCE: usize = 2;
const T_CLOCK: usize = 3;
const T_HOVER_DWELL: usize = 4;
const T_HOVER_LEAVE: usize = 5;
const T_PERMISSION: usize = 6;
/// Nochi idle motion (blink, glance).
const T_PET: usize = 7;
/// Delay before the first idle motion after Nochi appears or changes mood.
const PET_FIRST_MS: u32 = 3000;
/// Permission card countdown, once per second while the card is shown.
const T_COUNTDOWN: usize = 8;
/// Live activity deferred because the previous text has not been shown long enough.
const T_ACTIVITY: usize = 9;
/// Live activity text stays visible at least this long, so it does not flicker during back-to-back tools.
const ACTIVITY_DWELL: Duration = Duration::from_millis(700);
/// "Thinking…" only shows if the agent has actually been idle this long between tools.
const THINKING_QUIET: Duration = Duration::from_millis(1000);
/// Pomodoro countdown: one-shot, fires when the displayed text changes or the phase ends.
const T_FOCUS: usize = 10;
/// Smart chips close themselves; deferred while hovered.
const T_CHIP: usize = 11;
const CHIP_MS: u32 = 6000;
const CHIP_HOVER_MS: u32 = 1500;
/// Deck keys or text are sent shortly (after focus moves / Ctrl+Alt is released).
const T_SEND: usize = 12;
/// Notch slides to the top edge of the target monitor after release.
const T_GLIDE: usize = 13;
/// Prayer-card adhan position updates every second, only while the Widgets tab is open.
const T_ADHAN: usize = 14;
/// "Music playing" state for Nochi and the equalizer only changes after being stable for this long.
const T_MUSIC: usize = 15;
const MUSIC_SETTLE: Duration = Duration::from_millis(1500);
/// Prayer reminder closes itself when its time is up, after `prayer::REMINDER_HOLD_MIN`.
const T_PRAYER: usize = 16;
/// Permission card, question and chip buttons become clickable only this long after the card appears
/// or changes, so a click meant for something else doesn't approve a card that just appeared.
const CLICK_GUARD: Duration = Duration::from_millis(600);
const GLIDE_MS: f32 = 320.0;
/// A just-paused song keeps its button in the notch this long, so it can be resumed.
const PAUSED_CONTROLS: Duration = Duration::from_secs(5 * 60);
/// Claude quota is requested at most every 5 minutes while an agent works, and every minute while the Agent tab is open.
const QUOTA_EVERY: Duration = Duration::from_secs(5 * 60);
const QUOTA_ON_VIEW: Duration = Duration::from_secs(60);
/// How long the Claude Code hooks status (for the Connect button) is trusted before it is read again.
const HOOKS_RECHECK: Duration = Duration::from_secs(5);

const HOTKEY_TOGGLE: i32 = 1;
const HOTKEY_ADD: i32 = 2;
const HOTKEY_FOCUS: i32 = 3;
/// Ctrl+Alt+1..0: deck keys 1..10 (id 10..19).
const HOTKEY_DECK: i32 = 10;
const CONTROLLER_CLASS: PCWSTR = w!("NotchController");
const GLANCE_MS: u32 = 4000;
const HOVER_DWELL_MS: u32 = 150;
const HOVER_LEAVE_MS: u32 = 350;
/// A tray icon click also counts as an "outside click"; do not immediately reopen the panel that was just closed.
const REOPEN_GUARD: Duration = Duration::from_millis(400);

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// Deck editor is recording a key combination; read by the message loop before keys reach the input field.
static RECORDING: AtomicBool = AtomicBool::new(false);

/// Panel view state that is not saved to file.
#[derive(Default)]
struct PanelUi {
    hover: Option<Hit>,
    scroll: usize,
    input_open: bool,
    preview: Vec<Parsed>,
    /// Note after saving; disappears once the user types again.
    notice: Option<String>,
    /// Input field waits for the panel animation to finish before appearing and taking focus.
    focus_pending: bool,
    tab: Tab,
    /// Day viewed in the Today tab when not today (see `viewed_day`).
    day: Option<Date>,
    /// Posisi gulir halaman Settings (logis).
    settings_scroll: f32,
    /// Scroll position of the session list in the Agent tab.
    agent_scroll: f32,
    /// Session opened in the Agent tab.
    agent_open: Option<String>,
    /// Active selection in the command bar.
    cmd_selected: usize,
    /// Project for "claude …" (index in the recent projects list), switched with Tab.
    cmd_project: usize,
    /// Deck tab page.
    deck_page: usize,
    /// Deck key editor currently open.
    editor: Option<Editor>,
}

/// Deck key being edited. `field`: field being typed (0 label, 1 main field, 2 folder).
struct Editor {
    index: Option<usize>,
    draft: Button,
    field: Option<u8>,
    recording: bool,
    error: Option<&'static str>,
}

/// Notch slide to the top edge of the monitor: from, to (window top-left, screen pixels), and start time.
#[derive(Clone, Copy)]
struct Glide {
    from: (i32, i32),
    to: (i32, i32),
    started: Instant,
}

enum Send {
    Keys(Vec<u16>),
    Text(String),
}

/// Deck dispatch waiting for focus to move or modifiers to be released.
struct PendingSend {
    what: Send,
    label: String,
    tries: u32,
}

/// What a command bar choice runs.
#[derive(Clone)]
enum CmdAction {
    Timer { secs: u64, label: String, focus: bool },
    Copy(String),
    Claude { prompt: String, dir: String },
    AddTasks(String),
    Open(Entry),
    Web(String),
    ResumeFocus,
    StartFocus(u64),
    ShowToday,
    /// City for the prayer schedule.
    SetCity(prayer::Place),
    Locate,
}

/// What a chip button runs.
#[derive(Clone)]
enum ChipAction {
    StartFocus(Option<u64>),
    OpenUrl(String),
    AddTask(String),
    CopyImage,
    SaveImage,
    Open(String),
    Reveal(String),
    ActivateClaude,
    OpenProject(String),
    /// Closes the prayer reminder and stops the adhan.
    PrayerDone,
}

/// Prayer reminder waiting to be dismissed: prayer `index` in `prayer::NAMES`, its time
/// (local minutes), and its chip. The chip caption follows the adhan that is playing (see `content`).
struct PrayerNow {
    index: usize,
    at: f64,
    chip: SmartChip,
}

/// Smart chip currently shown.
struct SmartChip {
    lead: Lead,
    kicker: String,
    warn: bool,
    title: String,
    buttons: Vec<(ChipButton, ChipAction)>,
    mood: Mood,
}

fn button(label: Option<&str>, icon: Option<Icon>, style: ButtonStyle) -> ChipButton {
    ChipButton { label: label.map(str::to_string), icon, style }
}

fn greeting(hour: u16) -> &'static str {
    match hour {
        5..=11 => "Good morning",
        12..=16 => "Good afternoon",
        _ => "Good evening",
    }
}

/// Windows user's first name: "jane.doe" → "Jane".
fn user_name() -> String {
    let raw = std::env::var("USERNAME").unwrap_or_default();
    let first = raw.split(['.', '_', ' ', '-']).next().unwrap_or("");
    let mut chars = first.chars();
    chars.next().map_or(String::new(), |c| c.to_uppercase().chain(chars).collect())
}

fn file_size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{} KB", bytes / 1024),
        1_048_576..=1_073_741_823 => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", bytes as f64 / 1_073_741_824.0),
    }
}

/// Finish time for a duration of `secs` from now, e.g. "10:42 AM".
fn ends_at(secs: u64) -> String {
    let now = clock::now();
    let minute = (now.hour as u64 * 60 + now.minute as u64 + secs.div_ceil(60)) % (24 * 60);
    clock::time12(&format!("{:02}:{:02}", minute / 60, minute % 60))
}

/// "25 min", "1 h 30 min", "90 s".
fn duration_words(secs: u64) -> String {
    match secs {
        0..=119 if !secs.is_multiple_of(60) => format!("{secs} s"),
        _ if secs < 3600 => format!("{} min", secs / 60),
        _ if secs.is_multiple_of(3600) => format!("{} h", secs / 3600),
        _ => format!("{} h {} min", secs / 3600, secs / 60 % 60),
    }
}

fn ago(minutes: u64) -> String {
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes} min ago"),
        _ => format!("{} h ago", minutes / 60),
    }
}

fn home_dir() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into())
}

/// What the agent asks through a card in the notch.
enum Ask {
    /// Permission to run a tool: Deny / Allow.
    Permission,
    /// Multiple-choice question (`AskUserQuestion`), answered one by one. `picked`: options
    /// marked for the multi-select question currently shown.
    Questions { list: Vec<Question>, index: usize, answers: BTreeMap<String, String>, picked: Vec<usize> },
    /// Plan waiting for approval (`ExitPlanMode`).
    Plan { excerpt: String },
}

/// Request waiting for an answer. Dropping `reply` = not answering (the agent uses its own dialog).
struct Pending {
    session: String,
    project: String,
    tool: String,
    action: String,
    /// Full text on the card, e.g. the whole command (`action` is only the truncated first line).
    text: String,
    reply: Sender<Reply>,
    expires: Instant,
    /// Initial wait limit, seconds; for the countdown line.
    wait: u64,
    ask: Ask,
    /// Card content taller than the canvas is scrolled this far (logical).
    scroll: f32,
}

/// Tool name and action shown on the card; also used to recognize `PostToolUse` from the same
/// tool.
fn ask_action(ev: &HookEvent) -> (String, String) {
    let tool = ev.tool.as_deref().unwrap_or("?");
    let action = match (tool, ev.summary.as_deref()) {
        ("Bash" | "PowerShell", Some(cmd)) => cmd.to_string(),
        (_, summary) => crate::agents::describe(tool, summary),
    };
    (tool.to_string(), action)
}

pub struct App {
    controller: HWND,
    island: Island,
    input: Input,
    tray: Tray,
    watcher: Watcher,
    settings: Settings,
    store: Store,
    env: Env,
    ui: PanelUi,
    painted: Painted,
    visual: Visual,
    shape: Shape,
    monitor: HMONITOR,
    toggle_key_ok: bool,
    outside_collapse_at: Option<Instant>,
    agents: Agents,
    pending: VecDeque<Pending>,
    /// Hover state when the content was last drawn; pill and Live show different content when hovered.
    drawn_hover: bool,
    /// Nochi's mood during the glance currently shown.
    glance_mood: Mood,
    /// Tasks already reminded today, with their date.
    reminded: (String, HashSet<u64>),
    /// Activity currently shown in Live and since when.
    live_text: String,
    live_shown_at: Instant,
    /// Latest agent activity and since when (may not be shown yet).
    live_target: String,
    live_target_at: Instant,
    /// Nochi idle motion timer is running.
    pet_timer: bool,
    /// Session currently shown by Live.
    live_focus: Option<String>,
    pomodoro: Pomodoro,
    /// Task being worked on during the pomodoro.
    focus_task: Option<u64>,
    focus_key_ok: bool,
    /// Song from Windows media controls, updated via `WM_APP_MEDIA`.
    media: Option<NowPlaying>,
    /// Apps and folders for the command bar; built when the command bar opens.
    index: Index,
    chip: Option<SmartChip>,
    /// When the screen was locked, for "Welcome back".
    locked_at: Option<Instant>,
    /// The notch itself just copied to the clipboard; do not make it a chip.
    own_clip_at: Option<Instant>,
    /// Last clipboard content that became a chip; the clipboard often changes twice for one copy.
    last_clip: Option<(Clip, Instant)>,
    /// Label of a focus session or timer from the command bar that is not a task.
    focus_label: Option<String>,
    /// Image from the image chip currently shown (DIB), for Copy and Save; released when the chip disappears.
    held_image: Option<Vec<u8>>,
    /// When the last song was paused.
    media_paused_at: Option<Instant>,
    /// When Claude quota was last requested; throttled so the endpoint is not called constantly.
    quota_at: Option<Instant>,
    /// Latest Codex quota from its session logs, and when it was read.
    codex: Option<Vec<codex_quota::Window>>,
    codex_at: Option<Instant>,
    /// Connected agents (Claude Code hooks, then the MCP server per `McpApp::ALL`) and when that was read,
    /// for the Connect buttons in the Agent tab; the config files are not read on every redraw.
    hooks: Cell<Option<(Instant, Option<bool>, [Option<bool>; 4])>>,
    /// When the MCP server entry was last copied from Settings; its button briefly says "Copied".
    mcp_copied: Option<Instant>,
    /// Card or chip shown last (its identity) and since when; for `CLICK_GUARD`.
    armed: Option<String>,
    armed_at: Instant,
    /// Displayed music state (Nochi nodding, equalizer), and since when the real state has differed.
    /// A browser with many YouTube tabs (Shorts, ads, hover previews) makes the media session flip
    /// constantly; without this delay Nochi changes face and motion over and over.
    music_shown: bool,
    music_change_at: Option<Instant>,
    /// History of copied text and when, newest first; in memory only.
    clips: VecDeque<(String, Instant)>,
    /// "Mute today": adhan is replaced by the normal sound until this date passes.
    adhan_muted: Option<Date>,
    /// "Use my location" is in progress, or a message explaining why it failed.
    locating: bool,
    location_error: Option<String>,
    /// Last prayer reminder already shown (date, index, early?), so it isn't shown twice.
    prayer_alerted: Option<(Date, usize, bool)>,
    /// Prayer reminder still shown until dismissed. Separate from `chip`: another chip
    /// may show briefly on top, then the reminder returns.
    prayer_now: Option<PrayerNow>,
    deck: Deck,
    /// App icon of each deck key per target, fetched once.
    deck_icons: RefCell<HashMap<String, Option<Thumb>>>,
    /// Last active app window (not the notch), target of the Keys and Text buttons from the panel.
    last_app: HWND,
    sending: Option<PendingSend>,
    /// "Drop to add" chip is shown while something is dragged over the notch.
    drop_chip: bool,
    /// Monitor when the notch started being dragged.
    move_from: Option<HMONITOR>,
    /// Being dragged (Nochi is carried).
    floating: bool,
    /// Still reluctant to move (not pulled far enough yet).
    resisting: bool,
    /// Sliding into place after release.
    glide: Option<Glide>,
}

/// Logs only in debug builds; in release no string is formatted at all.
macro_rules! log {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!("[notch] {}", format!($($arg)*));
        }
    };
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&mut App) -> Result<()>) {
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => {
            if let Some(app) = guard.as_mut() {
                if let Err(e) = f(app) {
                    log!("error: {e}");
                }
            }
        }
        Err(_) => log!("event skipped: state is busy"),
    });
}

fn set_timer(hwnd: HWND, id: usize, ms: u32) {
    unsafe { SetTimer(Some(hwnd), id, ms, None) };
}

fn kill_timer(hwnd: HWND, id: usize) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
    }
}

pub fn start(instance: HINSTANCE) -> Result<()> {
    let app = App::new(instance)?;
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    with_app(|a| {
        a.sync_deck_hotkeys();
        a.evaluate();
        a.apply(false)?;
        a.maybe_greet(false)
    });
    Ok(())
}

/// Cleans up the tray icon and hooks before exit.
pub fn shutdown() {
    APP.with(|cell| drop(cell.borrow_mut().take()));
}

/// Commands from the command line or a second instance:
/// `toggle`, `expand`, `collapse`, `input`, `glance:<text>`, `add:<sentence>`.
pub fn command(cmd: &str) {
    with_app(|a| a.command(cmd));
}

/// Sends a command to the already running instance.
pub fn forward(cmd: &str) -> bool {
    unsafe {
        let Ok(hwnd) = FindWindowW(CONTROLLER_CLASS, None) else { return false };
        let data: Vec<u16> = cmd.encode_utf16().collect();
        let cds = COPYDATASTRUCT { dwData: 1, cbData: (data.len() * 2) as u32, lpData: data.as_ptr() as *mut _ };
        SendMessageW(hwnd, WM_COPYDATA, None, Some(LPARAM(&cds as *const _ as isize)));
        true
    }
}

/// Called by the message loop before `TranslateMessage`. Enter in the input field saves a task;
/// Esc is swallowed here (handled by the keyboard hook) so the EDIT control does not beep.
pub fn pre_translate(msg: &MSG) -> bool {
    if !input::is_edit(msg.hwnd) {
        return false;
    }
    if RECORDING.load(Ordering::Relaxed) && matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN) {
        // Recording a combination for a deck key: the keys are not typed into the field.
        let vk = msg.wParam.0 as u16;
        if !deck::vk::is_modifier(vk) {
            let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
            let mods = down(VK_CONTROL) as usize | (down(VK_SHIFT) as usize) << 1 | (down(VK_MENU) as usize) << 2 | ((down(VK_LWIN) || down(VK_RWIN)) as usize) << 3;
            watch::post(WM_APP_DECK_KEY, vk as usize, mods as isize);
        }
        return true;
    }
    if msg.message != WM_KEYDOWN {
        return false;
    }
    match VIRTUAL_KEY(msg.wParam.0 as u16) {
        VK_RETURN => {
            watch::post(WM_APP_INPUT_SUBMIT, 0, 0);
            true
        }
        VK_ESCAPE => true,
        // For the command bar: move the selection and switch project. The single-line field does not use them.
        VK_UP | VK_DOWN | VK_TAB => {
            watch::post(WM_APP_INPUT_KEY, msg.wParam.0, 0);
            true
        }
        _ => false,
    }
}

/// Second row of the other-day header: "4 done", "4 done · 1 open", "2 planned".
fn day_detail(done: usize, open: usize, past: bool) -> String {
    match (done, open) {
        (0, 0) if past => "Nothing done".into(),
        (0, 0) => "Nothing planned".into(),
        (d, 0) => format!("{d} done"),
        (0, o) if past => format!("{o} open"),
        (0, o) => format!("{o} planned"),
        (d, o) => format!("{d} done \u{00B7} {o} open"),
    }
}

fn chip_for(t: &Task, today: &str) -> Option<Chip> {
    if t.done {
        None
    } else if t.due.as_str() < today {
        Some(Chip { text: "Overdue".into(), warn: true })
    } else {
        t.time.as_deref().map(|hm| Chip { text: clock::time12(hm), warn: false })
    }
}

/// "1 task" / "3 tasks".
/// Kind of copied text: link, terminal command, or plain text.
fn clip_kind(text: &str) -> ClipKind {
    let t = text.trim();
    if command::link(t).is_some() {
        return ClipKind::Link;
    }
    const COMMANDS: [&str; 22] = [
        "git", "npm", "npx", "pnpm", "yarn", "cargo", "cd", "ls", "dir", "docker", "kubectl", "python", "pip", "node", "curl", "ssh", "winget", "choco", "dotnet", "go",
        "rustup", "claude",
    ];
    let first = t.split_whitespace().next().unwrap_or("");
    let one_line = !t.contains('\n');
    if one_line && (COMMANDS.contains(&first) || t.starts_with("./") || t.starts_with("$ ") || t.contains(" && ")) {
        ClipKind::Command
    } else {
        ClipKind::Text
    }
}

/// "now", "2m", "3h", "2d".
fn short_age(secs: u64) -> String {
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

/// "2:38".
fn mm_ss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn tasks(n: usize) -> String {
    format!("{n} task{}", if n == 1 { "" } else { "s" })
}

fn preview_of(p: &Parsed) -> Preview {
    let time = p.time.map(|(h, m)| clock::time12(&format!("{h:02}:{m:02}")));
    let chip = match (p.tomorrow, time) {
        (true, Some(t)) => Some(format!("Tomorrow {t}")),
        (true, None) => Some("Tomorrow".into()),
        (false, t) => t,
    };
    Preview { title: p.title.clone(), chip }
}

impl App {
    fn new(instance: HINSTANCE) -> Result<Self> {
        unsafe {
            let wc = WNDCLASSW {
                hInstance: instance,
                lpszClassName: CONTROLLER_CLASS,
                lpfnWndProc: Some(controller_wndproc),
                ..Default::default()
            };
            RegisterClassW(&wc);
            // Hidden window: receiver for tray, hotkey, timer, and hook messages.
            let controller = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CONTROLLER_CLASS,
                w!("Notch Controller"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )?;
            watch::set_controller(controller);
            pipe::start();
            media::start();
            chips::listen(controller);
            TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);

            let settings = Settings::load();
            let monitor = initial_monitor(&settings);
            let placement = display::placement(monitor).unwrap_or(FALLBACK_PLACEMENT);
            let island = Island::create(instance, placement)?;
            let input = Input::create(instance)?;
            let tray = Tray::add(controller)?;
            let hotkey = |id, key: char| RegisterHotKey(Some(controller), id, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, key as u32).is_ok();
            let toggle_key_ok = hotkey(HOTKEY_TOGGLE, 'N');
            if !hotkey(HOTKEY_ADD, 'T') {
                log!("Ctrl+Alt+T is taken by another app");
            }
            let focus_key_ok = hotkey(HOTKEY_FOCUS, 'P');
            if !focus_key_ok {
                log!("Ctrl+Alt+P is taken by another app");
            }
            let mut watcher = Watcher::start();
            watcher.follow(watch::foreground());
            let mut settings = settings;
            // First-time deck: from the previously pinned shortcuts, plus a few examples.
            let deck = match Deck::load() {
                Some(d) => d,
                None => {
                    let pins: Vec<(String, String)> = settings.pins.drain(..).map(|p| (p.name, p.target)).collect();
                    let d = Deck::starter(&pins, launcher::downloads_dir().map(|p| p.display().to_string()));
                    let _ = d.save();
                    let _ = settings.save();
                    d
                }
            };

            let app = Self {
                controller,
                island,
                input,
                tray,
                watcher,
                settings,
                store: {
                    // History past its limit is dropped once at startup.
                    let mut s = Store::open_default();
                    if s.prune(clock::today()) > 0 {
                        let _ = s.save();
                    }
                    s
                },
                env: Env::default(),
                ui: PanelUi::default(),
                painted: Painted::default(),
                visual: Visual::Hidden,
                shape: shape_for(&Visual::Hidden, 0.0, false, 0.0),
                monitor,
                toggle_key_ok,
                outside_collapse_at: None,
                agents: Agents::default(),
                pending: VecDeque::new(),
                drawn_hover: false,
                glance_mood: Mood::Happy { hop: true },
                reminded: (String::new(), HashSet::new()),
                live_text: String::new(),
                live_shown_at: Instant::now(),
                live_target: String::new(),
                live_target_at: Instant::now(),
                pet_timer: false,
                live_focus: None,
                pomodoro: Pomodoro::default(),
                focus_task: None,
                focus_key_ok,
                media: None,
                index: Index::default(),
                chip: None,
                locked_at: None,
                quota_at: None,
                codex: None,
                codex_at: None,
                hooks: Cell::new(None),
                mcp_copied: None,
                music_shown: false,
                music_change_at: None,
                armed: None,
                armed_at: Instant::now(),
                clips: VecDeque::new(),
                adhan_muted: None,
                locating: false,
                location_error: None,
                prayer_alerted: None,
                prayer_now: None,
                own_clip_at: None,
                last_clip: None,
                focus_label: None,
                held_image: None,
                media_paused_at: None,
                deck,
                deck_icons: RefCell::new(HashMap::new()),
                last_app: watch::foreground(),
                sending: None,
                drop_chip: false,
                move_from: None,
                floating: false,
                resisting: false,
                glide: None,
            };
            dragdrop::register(app.island.hwnd);
            app.schedule_clock();
            Ok(app)
        }
    }

    fn placement(&self) -> Placement {
        display::placement(self.monitor).unwrap_or(FALLBACK_PLACEMENT)
    }

    /// Reads the active window's state: fullscreen? occupying the notch area?
    fn evaluate(&mut self) {
        self.env.rest_shy = self.settings.size == NotchSize::Shy;
        let fg = watch::foreground();
        if watch::is_transient(fg) {
            return;
        }
        let p = self.placement();
        let zone = p.window_for(&shape_for(&Visual::Pill, 0.0, false, 0.0));
        let (fullscreen, shy) = watch::classify(fg, &p.monitor, &zone);
        self.env.fullscreen = self.settings.hide_fullscreen && (fullscreen || watch::presentation_mode());
        self.env.shy = self.settings.shy && shy;
    }

    // ---- Data panel ------------------------------------------------------------------------

    /// Visible rows: (task, total, completed count, above, below).
    fn visible_tasks(&self, today: Date) -> (Vec<&Task>, usize, usize, usize, usize) {
        let all = self.day_tasks(today);
        let total = all.len();
        let done = all.iter().filter(|t| t.done).count();
        let fit = layout::task_rows_fit(self.preview_lines(), self.media.is_some());
        let scroll = self.ui.scroll.min(total.saturating_sub(fit));
        let end = (scroll + fit).min(total);
        (all[scroll..end].to_vec(), total, done, scroll, total - end)
    }

    /// Other day being viewed in the Today tab; `None` for today, including when the
    /// chosen day became today after midnight.
    fn viewed_day(&self, today: Date) -> Option<Date> {
        self.ui.day.filter(|d| *d != today)
    }

    /// Tasks for the Today tab list: today's view, or those of the other day being viewed.
    fn day_tasks(&self, today: Date) -> Vec<&Task> {
        match self.viewed_day(today) {
            Some(d) => self.store.on_day(d),
            None => self.store.today(today),
        }
    }

    /// Summary of the other day for the Today tab header.
    fn day_view(&self, today: Date) -> Option<DayView> {
        let d = self.viewed_day(today)?;
        let all = self.store.on_day(d);
        let done = all.iter().filter(|t| t.done).count();
        Some(DayView { label: d.label(), detail: day_detail(done, all.len() - done, d < today), past: d < today })
    }

    /// Switch to another day in the Today tab (`None` = today). The input field is always for today, so
    /// it is closed when viewing another day.
    fn select_day(&mut self, day: Option<Date>) -> Result<()> {
        if day.is_some() {
            self.close_input();
        }
        self.ui.day = day;
        self.ui.scroll = 0;
        self.refresh()
    }

    /// Shift the week strip by a week from the day being viewed.
    fn step_week(&mut self, forward: bool) -> Result<()> {
        let today = clock::today();
        match tasks::week_step(self.viewed_day(today).unwrap_or(today), today, forward) {
            Some(d) => self.select_day((d != today).then_some(d)),
            None => Ok(()),
        }
    }

    /// Preview row above the list: `None` if the input field is closed or still empty.
    fn preview_lines(&self) -> Option<usize> {
        if !self.ui.input_open {
            return None;
        }
        layout::preview_lines(self.ui.preview.len(), self.ui.notice.is_some())
    }

    /// Session rows for the Agent tab: waiting and working ones first.
    fn agent_rows(&self) -> Vec<AgentRow> {
        self.agents
            .sessions_by_state()
            .iter()
            .map(|s| AgentRow {
                project: s.project.clone(),
                prompt: s.prompt.clone(),
                history: s.history.clone(),
                open: self.ui.agent_open.as_deref() == Some(s.id.as_str()),
                activity: s.activity.clone(),
                last: s.last.clone(),
                state: s.state,
                steps: s.steps,
                mcp: s.id.starts_with("mcp:"),
                minutes_ago: s.updated.elapsed().as_secs() / 60,
                minutes_working: s.started.elapsed().as_secs() / 60,
            })
            .collect()
    }

    /// Scroll bounds of the session list.
    fn agent_max_scroll(&self) -> f32 {
        let list = layout::agent_list_height(&self.agent_rows());
        (list - layout::agent_view_height(list, self.meter_count())).max(0.0)
    }

    /// Height of the current card so all its text is visible; the shape is capped in height
    /// kanvas, sisanya digulir.
    fn card_height(&self) -> f32 {
        self.island.card_height(&self.card_content())
    }

    /// Content of the front card: permission, question, or plan.
    fn card_content(&self) -> Content {
        let Some(p) = self.pending.front() else { return Content::Blank };
        let remaining = p.expires.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u64;
        match &p.ask {
            Ask::Permission => Content::Permission {
                project: p.project.clone(),
                tool: p.tool.clone(),
                action: p.text.clone(),
                queued: self.pending.len() - 1,
                remaining,
                wait: p.wait,
                hover: self.ui.hover,
                scroll: p.scroll,
            },
            Ask::Questions { list, index, picked, .. } => {
                let q = &list[*index];
                Content::Question {
                    project: p.project.clone(),
                    header: q.header.clone(),
                    text: q.question.clone(),
                    options: q.options.iter().enumerate().map(|(i, o)| (o.label.clone(), o.description.clone(), picked.contains(&i))).collect(),
                    multi: q.multi,
                    step: (*index + 1, list.len()),
                    remaining,
                    wait: p.wait,
                    hover: self.ui.hover,
                    scroll: p.scroll,
                }
            }
            Ask::Plan { excerpt } => Content::Plan { project: p.project.clone(), excerpt: excerpt.clone(), remaining, wait: p.wait, hover: self.ui.hover, scroll: p.scroll },
        }
    }

    /// Usage meter rows that have data.
    fn meter_count(&self) -> usize {
        paint::meters(self.agents.latest_context().map(|(t, _)| t), self.usage().as_ref()).len()
    }

    /// Usage for the Agent tab; without the 5-hour and weekly quota while "Claude usage" is off, and
    /// the Codex quota only while "Codex usage" is on.
    fn usage(&self) -> Option<agents::Usage> {
        let codex = if self.settings.codex_usage { self.codex.clone().unwrap_or_default() } else { Vec::new() };
        let mut u = match self.agents.usage.clone() {
            Some(u) => u,
            None if !codex.is_empty() => agents::Usage::default(),
            None => return None,
        };
        u.codex = codex;
        if !self.settings.claude_usage {
            u.five_hour_pct = None;
            u.seven_day_pct = None;
        }
        Some(u)
    }

    /// Connect and Show usage buttons, only while the Agent tab is open and has no sessions.
    fn agent_setup(&self, rows: &[AgentRow]) -> AgentSetup {
        if self.ui.tab != Tab::Agent || !rows.is_empty() {
            return AgentSetup::default();
        }
        let (claude, mcp) = match self.hooks.get().filter(|(t, ..)| t.elapsed() < HOOKS_RECHECK) {
            Some((_, c, m)) => (c, m),
            None => {
                let (c, m) = (agent_setup::claude_connected(), McpApp::ALL.map(agent_setup::mcp_connected));
                self.hooks.set(Some((Instant::now(), c, m)));
                (c, m)
            }
        };
        let all = std::iter::once((Setting::Claude, claude)).chain(McpApp::ALL.into_iter().map(Setting::Mcp).zip(mcp));
        let (mut connect, mut connected) = (Vec::new(), false);
        for (setting, state) in all {
            match state {
                Some(false) => connect.push(setting),
                Some(true) => connected = true,
                None => {}
            }
        }
        let codex = McpApp::ALL.into_iter().zip(mcp).any(|(app, state)| app == McpApp::Codex && state.is_some());
        AgentSetup { connect, connected, usage: !self.settings.claude_usage, codex_usage: codex && !self.settings.codex_usage }
    }

    fn panel_height(&self) -> f32 {
        match self.ui.tab {
            Tab::Today => {
                let (rows, ..) = self.visible_tasks(clock::today());
                layout::today_height(rows.len(), self.preview_lines(), self.media.is_some())
            }
            Tab::Agent => {
                let rows = self.agent_rows();
                let setup = self.agent_setup(&rows);
                let list = if setup.any() { layout::agent_setup_height(&setup) } else { layout::agent_list_height(&rows) };
                layout::agent_height(list, self.meter_count())
            }
            Tab::Focus => layout::focus_height(),
            Tab::Widgets => layout::widgets_height(
                self.settings.prayer_widget.then_some(self.settings.prayer.is_some()),
                self.settings.clip_history.then_some(self.clips.len()),
            ),
            Tab::Settings => layout::settings_height(),
            Tab::Command => layout::command_height(&self.command_items().0),
            Tab::Deck if self.ui.editor.is_some() => geometry::CANVAS_H,
            Tab::Deck => {
                // Empty deck: one more row for the hint.
                let page = self.deck_page();
                let rows = if page.grid.keys.is_empty() { 2 } else { layout::deck_rows(&page.grid, layout::DECK_ROWS) };
                layout::deck_height(rows, page.pages)
            }
        }
    }

    /// Next task for the hovered pill: (caption, overdue?, title, right side).
    fn next_task(&self, today: Date) -> Content {
        let all = self.store.today(today);
        let iso = today.iso();
        let open: Vec<&Task> = all.iter().copied().filter(|t| !t.done).collect();
        let side = (!open.is_empty()).then(|| format!("{} left", open.len()));
        // Overdue first, then the earliest time, then the rest.
        let next = open.iter().min_by_key(|t| (t.due.as_str() >= iso.as_str(), t.time.clone().unwrap_or_else(|| "99:99".into())));
        match next {
            Some(t) if t.due < iso => Content::Next { kicker: "Overdue".into(), warn: true, title: t.title.clone(), side },
            Some(t) => {
                let kicker = t.time.as_deref().map_or("Next".to_string(), |hm| format!("Next \u{00B7} {}", clock::time12(hm)));
                Content::Next { kicker, warn: false, title: t.title.clone(), side }
            }
            None if all.is_empty() => Content::Next { kicker: "Today".into(), warn: false, title: "Nothing planned".into(), side: None },
            None => Content::Next { kicker: "Today".into(), warn: false, title: "All clear".into(), side: None },
        }
    }

    /// Monday–Sunday strip for the week of the viewed day, marking days with open tasks
    /// or (past days) completed tasks.
    fn week(&self, today: Date) -> Vec<WeekDay> {
        const LETTERS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];
        let viewed = self.viewed_day(today).unwrap_or(today);
        let open = self.store.open_due_dates();
        let done = self.store.done_dates();
        let mut day = viewed.monday();
        (0..7)
            .map(|i| {
                let d = day;
                day = day.next();
                let iso = d.iso();
                let busy = open.contains(iso.as_str()) || (d < today && done.contains(iso.as_str()));
                WeekDay { letter: LETTERS[i], day: d.d, today: d == today, past: d < today, busy, selected: d == viewed }
            })
            .collect()
    }

    /// Title of the task being worked on, if any.
    fn focus_task_title(&self) -> Option<String> {
        let task = self.focus_task.and_then(|id| self.store.today(clock::today()).into_iter().find(|t| t.id == id).map(|t| t.title.clone()));
        task.or_else(|| self.focus_label.clone())
    }

    /// "Focus 2 of 4", "Short break".
    fn focus_phase_label(&self) -> String {
        let d = &self.settings.pomodoro;
        match self.pomodoro.phase() {
            Phase::Focus => format!("Focus {} of {}", self.pomodoro.done() % d.every + 1, d.every),
            Phase::ShortBreak => "Short break".into(),
            Phase::LongBreak => "Long break".into(),
            Phase::Timer => "Timer".into(),
        }
    }

    fn focus_content(&self) -> Content {
        let now = Instant::now();
        let d = &self.settings.pomodoro;
        let left = self.pomodoro.remaining(now, d);
        let paused = self.pomodoro.is_paused();
        let hover = self.detailed(&Visual::Focus);
        let mut kicker = self.focus_phase_label();
        if paused {
            kicker = format!("Paused \u{00B7} {kicker}");
        } else if let (Phase::Focus | Phase::Timer, Some(t)) = (self.pomodoro.phase(), self.focus_task_title()) {
            kicker = format!("{kicker} \u{00B7} {t}");
        }
        Content::Focus {
            kicker,
            // Compact: per minute. Hovered or paused (not moving): with seconds.
            // Seconds only when truly hovered (Full size stays per minute, to save CPU).
            time: if self.env.hovering || paused { pomodoro::clock(left) } else { pomodoro::minutes(left) },
            progress: self.pomodoro.progress(now, d),
            paused,
            dim: paused || matches!(self.pomodoro.phase(), Phase::ShortBreak | Phase::LongBreak),
            hover,
        }
    }

    fn focus_view(&self) -> FocusView {
        let now = Instant::now();
        let d = &self.settings.pomodoro;
        let active = self.pomodoro.is_active();
        let paused = self.pomodoro.is_paused();
        let phase = self.focus_phase_label().to_uppercase();
        FocusView {
            kicker: if active && !paused { phase } else if active { format!("PAUSED \u{00B7} {phase}") } else { format!("READY \u{00B7} {phase}") },
            time: pomodoro::clock(self.pomodoro.remaining(now, d)),
            progress: self.pomodoro.progress(now, d),
            active,
            paused,
            dim: paused || matches!(self.pomodoro.phase(), Phase::ShortBreak | Phase::LongBreak),
            task: self.focus_task_title(),
            can_link: self.store.today(clock::today()).iter().any(|t| !t.done),
            // During the long break all dots are filled; the count returns to zero afterwards.
            done: if self.pomodoro.phase() == Phase::LongBreak { d.every } else { self.pomodoro.done() % d.every },
            every: d.every,
            hotkey: self.focus_key_ok,
            lengths: *d,
        }
    }

    fn shape_of(&self, visual: &Visual) -> Shape {
        let glance_w = match visual {
            Visual::Glance(text) => self.island.glance_width(text),
            _ => 0.0,
        };
        let panel_h = match visual {
            Visual::Expanded => self.panel_height(),
            Visual::Permission => self.card_height(),
            _ => 0.0,
        };
        let shape = shape_for(visual, glance_w, self.detailed(visual), panel_h);
        // Media buttons need room: a hovered notch is slightly wider while a song is playing.
        match visual {
            Visual::Pill if self.env.hovering && self.media_recent().is_some() => Shape { w: shape.w + 40.0, ..shape },
            // Marker for the deck button that is on.
            Visual::Pill if !self.detailed(visual) && self.deck.buttons.iter().any(|b| b.on) => Shape { w: shape.w + 18.0, ..shape },
            // Live gets a song column on the right.
            Visual::Live if self.env.hovering && self.media_recent().is_some() => Shape { w: shape.w + 80.0, ..shape },
            _ => shape,
        }
    }

    /// Two-row view (as when hovered). Always in Full size; Shy only grows to a pill
    /// when the pill is hovered, but agent and focus details can still be opened.
    fn detailed(&self, visual: &Visual) -> bool {
        match self.settings.size {
            NotchSize::Full => true,
            NotchSize::Shy if *visual == Visual::Pill => false,
            _ => self.env.hovering,
        }
    }

    /// Song currently playing, or just paused (so it can be resumed from the notch).
    fn media_recent(&self) -> Option<&NowPlaying> {
        self.media.as_ref().filter(|m| m.playing || self.media_paused_at.is_some_and(|t| t.elapsed() < PAUSED_CONTROLS))
    }

    fn media_controls(&self) -> Option<MediaControls> {
        self.media_recent().map(|m| MediaControls { playing: m.playing, can_prev: m.can_prev, can_next: m.can_next, hover: self.ui.hover })
    }

    /// The hovered compact notch shows buttons that can be clicked and highlighted.
    fn hover_controls(&self) -> bool {
        matches!(self.visual, Visual::Pill | Visual::Live) && self.env.hovering && self.media_recent().is_some()
    }

    fn content(&self, visual: &Visual) -> Content {
        let now = clock::now();
        let today = now.date();
        match visual {
            Visual::Hidden => Content::Blank,
            Visual::Peek => Content::Peek,
            // Media buttons only when truly hovered; Full size without hover shows tasks.
            Visual::Pill if self.detailed(visual) => match (self.env.hovering, self.media_recent(), self.media_controls()) {
                (true, Some(m), Some(controls)) => Content::NowPlaying { title: m.title.clone(), artist: m.artist.clone(), controls },
                _ => self.next_task(today),
            },
            Visual::Pill => {
                let all = self.store.today(today);
                let music = self.music_shown;
                let flag = self.deck.buttons.iter().position(|b| b.on).map(|i| (paint::glyph_icon(self.deck.buttons[i].glyph), i));
                Content::Pill { done: all.iter().filter(|t| t.done).count(), total: all.len(), music, flag }
            }
            Visual::Glance(text) => {
                let (title, detail) = glance_parts(text);
                Content::Glance { title, detail }
            }
            Visual::Chip => match self.shown_chip() {
                Some(c) => {
                    // Prayer reminder: whether the caption follows the playing adhan.
                    let (kicker, warn) = match &self.prayer_now {
                        Some(p) if self.chip.is_none() => {
                            let playing = adhan::playing();
                            (prayer::reminder_text(p.index, p.at, playing).0, playing)
                        }
                        _ => (c.kicker.clone(), c.warn),
                    };
                    Content::SmartChip(Box::new(ChipView {
                        lead: c.lead.clone(),
                        kicker,
                        warn,
                        title: c.title.clone(),
                        buttons: c.buttons.iter().map(|(b, _)| b.clone()).collect(),
                        hover: self.ui.hover,
                    }))
                }
                None => Content::Blank,
            },
            Visual::Live => match self.live_session() {
                Some(s) => Content::Live {
                    activity: if self.live_text.is_empty() { s.activity.clone() } else { self.live_text.clone() },
                    project: s.project.clone(),
                    others: self.agents.active_count().saturating_sub(1),
                    minutes: s.started.elapsed().as_secs() / 60,
                    waiting: s.state == AgentState::Waiting,
                    hover: self.detailed(visual),
                    music: self.agent_music(visual),
                    controls: if self.env.hovering { self.media_controls() } else { None },
                    song: self.media_recent().map(|m| (m.title.clone(), m.artist.clone())),
                },
                None => Content::Blank,
            },
            Visual::Focus => self.focus_content(),
            Visual::Permission => self.card_content(),
            Visual::Expanded => {
                let iso = today.iso();
                let (tasks, total, done, more_above, more_below) = self.visible_tasks(today);
                let rows = tasks
                    .iter()
                    .map(|t| Row { id: t.id, title: t.title.clone(), done: t.done, chip: chip_for(t, &iso) })
                    .collect();
                let overdue = self.store.today(today).iter().filter(|t| !t.done && t.due < iso).count();
                let agents = self.agent_rows();
                Content::Expanded(Box::new(Panel {
                    tab: self.ui.tab,
                    week: self.week(today),
                    day: self.day_view(today),
                    week_prev: tasks::week_step(self.viewed_day(today).unwrap_or(today), today, false).is_some(),
                    week_next: tasks::week_step(self.viewed_day(today).unwrap_or(today), today, true).is_some(),
                    overdue,
                    agent_working: self.agents.active().is_some(),
                    rows,
                    more_above,
                    more_below,
                    done,
                    total,
                    hover: self.ui.hover,
                    input: self.ui.input_open.then(|| self.ui.preview.iter().map(preview_of).collect()),
                    notice: self.ui.notice.clone(),
                    agent_setup: self.agent_setup(&agents),
                    agents,
                    agent_scroll: self.ui.agent_scroll,
                    usage: self.usage(),
                    context: self.agents.latest_context().map(|(t, _)| t),
                    // Name from statusLine if present; otherwise from the model ID in the transcript.
                    model: self
                        .agents
                        .usage
                        .as_ref()
                        .and_then(|u| u.model.clone())
                        .or_else(|| self.agents.latest_context().and_then(|(_, m)| m).map(agents::model_name)),
                    // Claude's registry and settings.json are only read while the Settings page is open.
                    settings: if self.ui.tab == Tab::Settings {
                        SettingsView {
                            shy: self.settings.shy,
                            hide_fullscreen: self.settings.hide_fullscreen,
                            follow_app: self.settings.monitor == MonitorMode::Follow,
                            autostart: autostart::is_enabled(),
                            chips: self.settings.chips,
                            claude: agent_setup::claude_connected(),
                            mcp: McpApp::ALL.map(agent_setup::mcp_connected),
                            mcp_copied: self.mcp_copied.is_some_and(|t| t.elapsed() < Duration::from_secs(3)),
                            usage: self.settings.claude_usage,
                            codex_usage: self.settings.codex_usage,
                            prayer_widget: self.settings.prayer_widget,
                            prayer_remind: self.settings.prayer_remind,
                            sound: self.settings.sound,
                            size: self.settings.size,
                            adhan: self.settings.adhan,
                            clip_history: self.settings.clip_history,
                        }
                    } else {
                        SettingsView::default()
                    },
                    settings_scroll: self.ui.settings_scroll,
                    focus: self.focus_view(),
                    media: self.media.clone(),
                    command: (self.ui.tab == Tab::Command).then(|| self.command_items().0),
                    deck: (self.ui.tab == Tab::Deck && self.ui.editor.is_none()).then(|| self.deck_page()),
                    editor: if self.ui.tab == Tab::Deck { self.editor_view() } else { None },
                    widgets: if self.ui.tab == Tab::Widgets { self.widgets_view() } else { WidgetsView::default() },
                    widgets_tab: self.widgets_on(),
                }))
            }
        }
    }

    // ---- Display ---------------------------------------------------------------------------

    fn draw(&mut self, visual: &Visual, shape: &Shape) -> Result<()> {
        if *visual == Visual::Expanded && self.ui.tab == Tab::Agent {
            self.maybe_quota(QUOTA_ON_VIEW);
        }
        // New card or chip (or the front card changed): its buttons are locked briefly.
        let armed = match visual {
            Visual::Permission => self.pending.front().map(|p| format!("{}\u{1F}{}\u{1F}{}", p.session, p.tool, p.action)),
            Visual::Chip => self.shown_chip().map(|c| format!("{}\u{1F}{}", c.kicker, c.title)),
            _ => None,
        };
        if armed != self.armed {
            self.armed = armed;
            self.armed_at = Instant::now();
        }
        self.settle_music();
        let content = self.content(visual);
        self.painted = self.island.draw(&content, shape)?;
        self.drawn_hover = self.env.hovering;
        self.island.set_pet(self.painted.pet, self.pet_mood(visual), self.agent_music(visual))?;
        // Idle motion is only scheduled here, not played: otherwise Nochi blinks every
        // time the content is redrawn (e.g. on every agent event). A timer already running is left alone.
        if !self.island.pet_wants_idle() {
            kill_timer(self.controller, T_PET);
            self.pet_timer = false;
        } else if !self.pet_timer {
            set_timer(self.controller, T_PET, PET_FIRST_MS);
            self.pet_timer = true;
        }
        if *visual == Visual::Permission {
            set_timer(self.controller, T_COUNTDOWN, 1000);
        }
        if *visual == Visual::Expanded && self.ui.tab == Tab::Widgets && adhan::playing() {
            set_timer(self.controller, T_ADHAN, 1000);
        }
        Ok(())
    }

    /// Nochi's mood for this view.
    fn pet_mood(&self, visual: &Visual) -> Mood {
        if self.floating {
            return if self.resisting { Mood::Reluctant } else { Mood::Carried };
        }
        let now = clock::now();
        let night = now.hour >= 23 || now.hour < 5;
        let agent = self.live_session().map(|s| (s.state, self.live_text == agents::THINKING));
        let focusing = self.pomodoro.is_active() && !self.pomodoro.is_paused() && self.pomodoro.phase() == Phase::Focus;
        match visual {
            Visual::Live => match agent {
                Some((AgentState::Waiting, _)) => Mood::Alert,
                Some((_, true)) => Mood::Thinking,
                _ => Mood::Focused,
            },
            Visual::Focus if focusing => Mood::Zen,
            Visual::Focus => Mood::Idle,
            // Peeking: still lets you know if the agent is working or needs you.
            Visual::Peek => match agent {
                Some((AgentState::Waiting, _)) => Mood::Alert,
                Some(_) => Mood::Focused,
                None if focusing => Mood::Zen,
                None if night => Mood::Sleepy,
                None => Mood::Idle,
            },
            Visual::Glance(_) => self.glance_mood,
            Visual::Chip => self.shown_chip().map_or(Mood::Idle, |c| c.mood),
            Visual::Permission => Mood::Alert,
            Visual::Pill if self.env.hovering => Mood::Curious,
            // Watching you type.
            Visual::Expanded if self.ui.input_open => Mood::Curious,
            Visual::Expanded if focusing && self.ui.tab == Tab::Focus => Mood::Zen,
            Visual::Pill => {
                let all = self.store.today(now.date());
                if !all.is_empty() && all.iter().all(|t| t.done) {
                    Mood::Happy { hop: false }
                } else if night {
                    Mood::Sleepy
                } else {
                    Mood::Idle
                }
            }
            _ => Mood::Idle,
        }
    }

    /// Agent works while music plays: equalizer on the right of Live, and Nochi nods.
    fn agent_music(&self, visual: &Visual) -> bool {
        matches!(visual, Visual::Live | Visual::Peek) && self.live_session().is_some() && self.music_shown
    }

    /// Syncs `music_shown` with the real media state, but only after that state has held for
    /// `MUSIC_SETTLE`; until then the `T_MUSIC` timer is set to check again.
    fn settle_music(&mut self) {
        let playing = self.media.as_ref().is_some_and(|m| m.playing);
        if playing == self.music_shown {
            self.music_change_at = None;
            return;
        }
        match self.music_change_at {
            None => {
                self.music_change_at = Some(Instant::now());
                set_timer(self.controller, T_MUSIC, MUSIC_SETTLE.as_millis() as u32 + 20);
            }
            Some(t) if t.elapsed() >= MUSIC_SETTLE => {
                log!("music shown: {playing}");
                self.music_shown = playing;
                self.music_change_at = None;
            }
            Some(_) => {}
        }
    }

    /// Spinning chrome ring, rocking crescent moon and moving equalizer where the
    /// painter decides.
    fn sync_ring(&mut self) -> Result<()> {
        self.island.set_rings(&self.painted.rings)?;
        self.island.set_marquee(self.painted.marquee.as_ref())?;
        self.island.set_moon(self.painted.moon)?;
        self.island.set_eq(self.painted.eq)
    }

    /// Applies the result of `decide`. `animate = false` forces a redraw without animation (e.g. monitor change).
    fn apply(&mut self, animate: bool) -> Result<()> {
        // While being carried, shape and window are managed by `island`; state changes wait until release.
        if self.floating || self.glide.is_some() {
            return Ok(());
        }
        let visual = decide(&self.env);
        let shape = self.shape_of(&visual);
        let visual_changed = visual != self.visual;
        // Pill and Live show different content when hovered, so a hover change also redraws.
        let hover_changed = matches!(visual, Visual::Pill | Visual::Live | Visual::Focus) && self.env.hovering != self.drawn_hover;
        if animate && !visual_changed && !hover_changed && shape == self.shape {
            return Ok(());
        }
        log!("apply {visual:?} {}x{} animate={animate}", shape.w, shape.h);
        let expanded = visual == Visual::Expanded;
        self.watcher.set_panel_hooks(expanded);
        let controls = matches!(visual, Visual::Pill | Visual::Live) && self.env.hovering && self.media_recent().is_some();
        island::set_panel_mode(expanded || controls || matches!(visual, Visual::Permission | Visual::Chip));
        island::set_movable(!expanded && !matches!(visual, Visual::Permission | Visual::Chip | Visual::Hidden));
        if !expanded {
            self.close_input();
            self.ui.hover = None;
        }
        kill_timer(self.controller, T_SETTLE);

        if visual == Visual::Hidden {
            self.painted = Painted::default();
            self.island.set_pet(None, Mood::Idle, false)?;
            self.island.hide();
        } else {
            let p = self.placement();
            let target = p.window_for(&shape);
            if !animate || !self.island.visible() {
                self.island.set_shape(&shape, None)?;
                self.island.set_window(target)?;
                self.draw(&visual, &shape)?;
                self.island.show();
                if self.ui.input_open {
                    self.ui.focus_pending = false;
                    self.sync_input(true);
                }
            } else {
                // Growing overshoots a little, shrinking does not.
                let spring = if shape.area() > self.shape.area() { Spring::EXPAND } else { Spring::COLLAPSE };
                // Window is enlarged first so the animation and overshoot are not clipped, then shrunk after it finishes.
                let current = self.island.window();
                let room = if (shape.w - self.shape.w).abs() < 0.5 && current.x == target.x {
                    p.while_resizing(&current, &target)
                } else {
                    p.while_animating(&current.union(&target))
                };
                self.island.set_window(room)?;
                let ms = self.island.set_shape(&shape, Some(spring))?;
                if visual_changed || hover_changed {
                    self.draw(&visual, &shape)?;
                    self.island.reveal()?;
                } else if expanded {
                    // Panel height changed (task added, input preview): content is redrawn without the appear effect.
                    self.draw(&visual, &shape)?;
                }
                set_timer(self.controller, T_SETTLE, ms + 40);
            }
        }
        self.visual = visual;
        self.shape = shape;
        self.schedule_focus();
        self.sync_ring()
    }

    /// Content changed (task, hover, preview, scroll): redraw, and animate if the size changes too.
    fn refresh(&mut self) -> Result<()> {
        if !matches!(self.visual, Visual::Peek | Visual::Pill | Visual::Live | Visual::Focus | Visual::Permission | Visual::Chip | Visual::Expanded) {
            return Ok(());
        }
        let visual = self.visual.clone();
        let shape = self.shape_of(&visual);
        if shape != self.shape {
            self.apply(true)?;
        } else {
            self.draw(&visual, &shape)?;
        }
        if !self.ui.focus_pending {
            self.sync_input(false);
        }
        self.sync_ring()
    }

    /// Session shown by Live: stays the same session while it is active, so the notch does not
    /// switch back and forth when several sessions work at once; once inactive, the most recent one.
    fn live_session(&self) -> Option<&agents::Session> {
        self.live_focus.as_deref().and_then(|id| self.agents.active_by_id(id)).or_else(|| self.agents.active())
    }

    /// Syncs the Live text with agent activity, but no faster than `ACTIVITY_DWELL`,
    /// and "Thinking…" only after the agent has been idle for `THINKING_QUIET`. Returns `true` if the
    /// displayed text changed; otherwise, a timer is set to retry.
    fn sync_live(&mut self) -> bool {
        let now = Instant::now();
        self.live_focus = self.live_session().map(|s| s.id.clone());
        // Between tools, progress reported by the agent (MCP) replaces "Thinking…".
        let target = self
            .live_session()
            .map(|s| match (&s.progress, s.activity == agents::THINKING) {
                (Some(p), true) => p.clone(),
                _ => s.activity.clone(),
            })
            .unwrap_or_default();
        if target != self.live_target {
            self.live_target = target.clone();
            self.live_target_at = now;
        }
        if target == self.live_text {
            kill_timer(self.controller, T_ACTIVITY);
            return false;
        }
        let dwell = if self.live_text.is_empty() || target.is_empty() {
            Duration::ZERO
        } else {
            ACTIVITY_DWELL.saturating_sub(now.duration_since(self.live_shown_at))
        };
        let quiet = if target == agents::THINKING { THINKING_QUIET.saturating_sub(now.duration_since(self.live_target_at)) } else { Duration::ZERO };
        let wait = dwell.max(quiet);
        if wait.is_zero() {
            self.live_text = target;
            self.live_shown_at = now;
            kill_timer(self.controller, T_ACTIVITY);
            true
        } else {
            set_timer(self.controller, T_ACTIVITY, wait.as_millis() as u32 + 10);
            false
        }
    }

    /// Agent or permission state changed: re-decide the view, then redraw its content if the view stays the same.
    fn update(&mut self) -> Result<()> {
        self.sync_live();
        self.env.live = self.agents.active().is_some();
        self.env.permission = !self.pending.is_empty();
        let before = self.visual.clone();
        self.apply(true)?;
        if self.visual == before {
            self.refresh()
        } else {
            Ok(())
        }
    }

    // ---- Agent -----------------------------------------------------------------------------

    fn on_agent(&mut self, incoming: Incoming) -> Result<()> {
        let now = Instant::now();
        let mut chip = None;
        log!("agent: {:?}", incoming.msg);
        match incoming.msg {
            Message::Mcp(m) => return self.on_mcp(m, incoming.reply),
            Message::Status(st) => self.agents.apply_status(&st),
            Message::Hook(ev) => {
                // Quota only changes while Claude is used, so it is refreshed following agent events.
                self.maybe_quota(QUOTA_EVERY);
                if let Some(dir) = &ev.cwd {
                    self.remember_project(dir);
                }
                // The card is dropped only if it was clearly answered elsewhere: the tool finished, or
                // the session's turn ended. Parallel tools and subagents in the same session do not close
                // a card that is still waiting. The sender is released too so the bridge finishes.
                let (tool, action) = ask_action(&ev);
                self.pending.retain(|p| {
                    p.session != ev.session
                        || match ev.event.as_str() {
                            "PostToolUse" => p.tool != tool || p.action != action,
                            "Stop" | "UserPromptSubmit" | "SessionEnd" => false,
                            _ => true,
                        }
                });
                // Permissions, questions, and plans that can be answered from the notch are shown as cards.
                let carded = ev.wants_reply() && incoming.reply.is_some();
                if let (true, Some(reply)) = (ev.wants_reply(), incoming.reply) {
                    let ask = match (&ev.questions, ev.is_permission()) {
                        (_, true) => Ask::Permission,
                        (Some(list), false) => Ask::Questions { list: list.clone(), index: 0, answers: BTreeMap::new(), picked: Vec::new() },
                        (None, false) => Ask::Plan { excerpt: ev.plan.clone().unwrap_or_else(|| "Claude has a plan ready.".into()) },
                    };
                    self.pending.push_back(Pending {
                        session: ev.session.clone(),
                        project: ev.project.clone(),
                        tool,
                        text: ev.command.clone().unwrap_or_else(|| action.clone()),
                        action,
                        reply,
                        expires: now + Duration::from_secs(ev.wait_secs()),
                        wait: ev.wait_secs(),
                        ask,
                        scroll: 0.0,
                    });
                    self.schedule_permission_timer();
                }
                match self.agents.apply(&ev, now) {
                    Some(Notice::Finished { project, steps, prompt, dir }) if !self.env.expanded && !self.env.fullscreen => {
                        let kicker = match steps {
                            0 => format!("Claude finished \u{00B7} {project}"),
                            n => format!("Claude finished \u{00B7} {project} \u{00B7} {n} {}", steps_word(n)),
                        };
                        if self.settings.chips {
                            let mut buttons = Vec::new();
                            if launcher::claude_window().is_some() {
                                buttons.push((button(Some("Review"), None, ButtonStyle::Primary), ChipAction::ActivateClaude));
                            }
                            if let Some(d) = dir {
                                buttons.push((button(None, Some(Icon::Code), ButtonStyle::Plain), ChipAction::OpenProject(d)));
                            }
                            let title = prompt.unwrap_or_else(|| "All done".into());
                            chip = Some(SmartChip { lead: Lead::Pet, kicker, warn: false, title, buttons, mood: Mood::Happy { hop: true } });
                        } else {
                            self.glance_mood = Mood::Happy { hop: true };
                            self.env.glance = Some(kicker);
                            set_timer(self.controller, T_GLANCE, GLANCE_MS);
                        }
                    }
                    // Question or plan waits for an answer in the agent app: notify once via a glance.
                    Some(Notice::NeedsYou { project, what }) if !carded && !self.env.expanded && !self.env.fullscreen => {
                        self.glance_mood = Mood::Alert;
                        self.env.glance = Some(format!("{what} · {project}"));
                        set_timer(self.controller, T_GLANCE, GLANCE_MS);
                    }
                    _ => {}
                }
            }
        }
        self.update()?;
        // After the agent state is updated, so the chip shows on top of the correct state.
        match chip {
            Some(c) => self.show_chip(c),
            None => Ok(()),
        }
    }

    // ---- Widget ----------------------------------------------------------------------------

    /// A widget is active; without one the Widgets tab is hidden.
    fn widgets_on(&self) -> bool {
        self.settings.prayer_widget || self.settings.clip_history
    }

    fn widgets_view(&self) -> WidgetsView {
        WidgetsView {
            prayer_on: self.settings.prayer_widget,
            prayer: self.prayer_view(),
            locating: self.locating,
            location_error: self.location_error.clone(),
            clips: self.settings.clip_history.then(|| {
                self.clips
                    .iter()
                    .map(|(text, at)| ClipRow { text: text.clone(), kind: clip_kind(text), age: short_age(at.elapsed().as_secs()) })
                    .collect()
            }),
        }
    }

    /// Today's schedule for the chosen city, in the Windows time zone.
    fn prayer_today(&self) -> Option<prayer::Times> {
        let p = self.settings.prayer.as_ref()?;
        Some(prayer::times(clock::today(), p.lat, p.lon, clock::utc_offset_hours()))
    }

    fn prayer_view(&self) -> Option<PrayerView> {
        let place = self.settings.prayer.as_ref()?;
        let t = self.prayer_today()?;
        let now = clock::now();
        let minutes = (now.hour as u32 * 60 + now.minute as u32) as f64;
        let (next, until) = prayer::next(&t, minutes);
        let until = until.round() as u64;
        Some(PrayerView {
            place: place.name.clone(),
            times: t.prayers.map(prayer::hm),
            next,
            passed: t.prayers.iter().filter(|p| p.ceil() <= minutes).count(),
            progress: {
                // From the previous prayer (before Fajr: yesterday's Isha) to the next prayer.
                let at = t.prayers[next].ceil();
                let at = if at < minutes { at + 24.0 * 60.0 } else { at };
                let prev = if next == 0 { t.prayers[4].ceil() - 24.0 * 60.0 } else { t.prayers[next - 1].ceil() };
                let prev = if minutes < prev { prev - 24.0 * 60.0 } else { prev };
                ((minutes - prev) / (at - prev).max(1.0)) as f32
            },
            hijri: prayer::hijri(clock::today()),
            adhan: adhan::playing().then(|| match adhan::progress() {
                Some((pos, len)) => format!("{} / {}", mm_ss(pos / 1000), mm_ss(len / 1000)),
                None => String::new(),
            }),
            until: if until < 1 { "now".into() } else if until < 60 { format!("in {until} min") } else { format!("in {}h {}m", until / 60, until % 60) },
        })
    }

    fn set_prayer_place(&mut self, place: prayer::Place) -> Result<()> {
        self.settings.prayer = Some(place);
        // Choosing a city means the user wants to see its schedule.
        self.settings.prayer_widget = true;
        self.location_error = None;
        self.prayer_alerted = None;
        self.save_settings();
        self.input.clear();
        self.close_input();
        self.ui.tab = Tab::Widgets;
        self.ui.hover = None;
        if self.env.expanded {
            self.refresh()
        } else {
            self.set_expanded(true)
        }
    }

    fn on_widget(&mut self, cmd: WidgetCmd) -> Result<()> {
        match cmd {
            WidgetCmd::City => {
                self.open_command()?;
                self.input.set_text("city ");
                Ok(())
            }
            WidgetCmd::Locate => {
                self.settings.prayer_widget = true;
                self.locating = true;
                self.location_error = None;
                location::request();
                self.ui.tab = Tab::Widgets;
                self.close_input();
                if self.env.expanded {
                    self.refresh()
                } else {
                    self.set_expanded(true)
                }
            }
            WidgetCmd::Clip(i) => {
                let Some(text) = self.clips.get(i).map(|(t, _)| t.clone()) else { return Ok(()) };
                self.own_clip_at = Some(Instant::now());
                if launcher::copy_text(self.controller, &text) {
                    self.remember_clip(text);
                }
                self.ui.hover = None;
                self.refresh()
            }
            WidgetCmd::MuteAdhan => {
                adhan::close();
                self.adhan_muted = Some(clock::today());
                self.ui.hover = None;
                self.refresh()
            }
            WidgetCmd::StopAdhan => {
                adhan::close();
                self.ui.hover = None;
                self.refresh()
            }
            WidgetCmd::ClearClips => {
                self.clips.clear();
                self.ui.hover = None;
                self.refresh()
            }
        }
    }

    fn on_location(&mut self) -> Result<()> {
        let Some(found) = location::take() else { return Ok(()) };
        self.locating = false;
        match found {
            Ok((lat, lon)) => {
                // Name of the nearest built-in city; if far from all of them, just the coordinates.
                let (city, km) = prayer::nearest(lat, lon);
                let name = match km {
                    k if k < 25.0 => city.to_string(),
                    k if k < 150.0 => format!("Near {city}"),
                    _ => format!("{lat:.2}, {lon:.2}"),
                };
                self.set_prayer_place(prayer::Place { name, lat, lon })
            }
            Err(e) => {
                self.location_error = Some(e.to_string());
                self.refresh()
            }
        }
    }

    /// Copied text goes to the front of the history (no duplicates).
    fn remember_clip(&mut self, text: String) {
        const MAX_CLIPS: usize = 3;
        self.clips.retain(|(c, _)| *c != text);
        self.clips.push_front((text, Instant::now()));
        self.clips.truncate(MAX_CLIPS);
    }

    /// Called every minute: glance 10 minutes before prayer time, then the reminder waits
    /// to be dismissed and sounds when the time comes.
    fn prayer_tick(&mut self) -> Result<()> {
        if !self.settings.prayer_widget || !self.settings.prayer_remind {
            return Ok(());
        }
        let Some(t) = self.prayer_today() else { return Ok(()) };
        let now = clock::now();
        let minutes = (now.hour as u32 * 60 + now.minute as u32) as f64;
        let Some((i, early)) = prayer::alert_at(&t, minutes) else { return Ok(()) };
        let key = (now.date(), i, early);
        if self.prayer_alerted == Some(key) {
            return Ok(());
        }
        self.prayer_alerted = Some(key);
        self.prayer_alert(i, t.prayers[i], early)
    }

    /// Reminder for prayer `i` (time `at`, local minutes): a silent glance 10 minutes before,
    /// at the time it sounds and the chip stays until dismissed (`show_prayer`).
    fn prayer_alert(&mut self, i: usize, at: f64, early: bool) -> Result<()> {
        let name = prayer::NAMES[i];
        if early {
            return self.show_glance(&format!("{name} in 10 min \u{00B7} {}", prayer::hm(at)), Mood::Curious);
        }
        // Full adhan if selected and the file exists; otherwise the normal sound.
        let muted = self.adhan_muted == Some(clock::today());
        let full = self.settings.sound && self.settings.adhan && !muted && adhan::play(self.controller);
        log!("adhan: {}", if full { "playing" } else { "not used" });
        if !full {
            self.play(Chime::Prayer);
        }
        self.show_prayer(i, at)
    }

    /// Reminder chime, unless turned off in Settings.
    fn play(&self, kind: Chime) {
        if self.settings.sound {
            let played = chime::play(kind);
            log!("chime {kind:?}: {}", if played { "played" } else { "refused" });
        }
    }

    // ---- Kuota Claude ----------------------------------------------------------------------

    /// Requests the Claude quota if the last request is older than `min`. While "Claude usage" is
    /// off, the Claude Code login is not read at all.
    fn maybe_quota(&mut self, min: Duration) {
        if self.settings.claude_usage && self.quota_at.is_none_or(|t| t.elapsed() >= min) {
            self.quota_at = Some(Instant::now());
            quota::refresh();
        }
        // Codex: local files only, so just read them directly.
        if self.settings.codex_usage && self.codex_at.is_none_or(|t| t.elapsed() >= min) {
            self.codex_at = Some(Instant::now());
            self.codex = codex_quota::read();
            log!("codex quota: {:?}", self.codex);
        }
    }

    fn on_quota(&mut self) -> Result<()> {
        let Some(q) = quota::take() else { return Ok(()) };
        log!("quota: {q:?}");
        self.agents.apply_quota(&q);
        if self.visual == Visual::Expanded && self.ui.tab == Tab::Agent {
            self.refresh()?;
        }
        Ok(())
    }

    // ---- MCP -------------------------------------------------------------------------------

    /// MCP tool calls from the agent. Questions and permission requests become cards waiting for
    /// your answer; the rest are answered immediately.
    fn on_mcp(&mut self, m: Mcp, reply: Option<Sender<Reply>>) -> Result<()> {
        let now = Instant::now();
        let answer = |reply: Option<Sender<Reply>>, text: String| {
            if let Some(r) = reply {
                let _ = r.send(Reply { text: Some(text), ..Reply::default() });
            }
        };
        // Claude Code already shows up via hooks; other agents are only visible from their MCP calls.
        if m.agent != "Claude Code" {
            let activity = match &m.call {
                McpCall::Notify { title, .. } => Some((title.clone(), AgentState::Done)),
                McpCall::AskUser { question, .. } => Some((format!("Asked: {question}"), AgentState::Waiting)),
                McpCall::RequestApproval { .. } => Some(("Needs your approval".to_string(), AgentState::Waiting)),
                McpCall::Progress { label, percent } => Some((
                    match percent {
                        Some(p) => format!("{label} \u{00B7} {:.0}%", p.clamp(0.0, 100.0)),
                        None => label.clone(),
                    },
                    AgentState::Working,
                )),
                McpCall::AddTask { .. } => Some(("Added tasks".to_string(), AgentState::Done)),
                McpCall::CompleteTask { .. } => Some(("Completed a task".to_string(), AgentState::Done)),
                McpCall::GetStatus => None,
            };
            if let Some((text, state)) = activity {
                self.agents.mcp_activity(&m.agent, text, state, now);
            }
        }
        match m.call {
            McpCall::Notify { title, detail } => {
                let kicker = match detail.as_deref().map(str::trim) {
                    Some(d) if !d.is_empty() => format!("{} \u{00B7} {d}", m.agent),
                    _ => m.agent.clone(),
                };
                let visible = !self.env.expanded && !self.env.fullscreen && !self.env.permission;
                answer(reply, if visible { "Shown in the notch.".into() } else { "The notch is busy (panel open, a card waiting, or fullscreen); the message was not shown.".into() });
                if !visible {
                    return Ok(());
                }
                self.show_chip(SmartChip { lead: Lead::Pet, kicker, warn: false, title, buttons: Vec::new(), mood: Mood::Happy { hop: true } })
            }
            McpCall::AskUser { question, options, multi } => {
                let Some(reply) = reply else { return Ok(()) };
                let list = vec![Question { question: question.clone(), header: None, options: options.into_iter().map(|label| Choice { label, description: None }).collect(), multi }];
                self.push_mcp_card(&m.agent, "ask_user", question, reply, Ask::Questions { list, index: 0, answers: BTreeMap::new(), picked: Vec::new() }, now)
            }
            McpCall::RequestApproval { action, detail } => {
                let Some(reply) = reply else { return Ok(()) };
                let action = match detail.as_deref().map(str::trim) {
                    Some(d) if !d.is_empty() => format!("{action} \u{2014} {d}"),
                    _ => action,
                };
                self.push_mcp_card(&m.agent, "approval", action, reply, Ask::Permission, now)
            }
            McpCall::Progress { label, percent } => {
                let text = match percent {
                    Some(p) => format!("{label} \u{00B7} {:.0}%", p.clamp(0.0, 100.0)),
                    None => label,
                };
                if m.agent != "Claude Code" || self.agents.progress(&text, now) {
                    answer(reply, "Shown in the notch's live view.".into());
                    return self.update();
                }
                answer(reply, "Shown briefly in the notch.".into());
                self.show_glance(&format!("{} \u{00B7} {text}", m.agent), Mood::Curious)
            }
            McpCall::AddTask { text } => {
                let titles: Vec<String> = parse(&text).into_iter().map(|p| p.title).collect();
                let (n, tomorrow) = self.add_from_text(&text);
                let when = if tomorrow > 0 { format!(" ({tomorrow} for tomorrow)") } else { String::new() };
                answer(reply, if n == 0 { "Nothing to add.".into() } else { format!("Added {}{when}: {}", tasks(n), titles.join("; ")) });
                self.refresh()
            }
            McpCall::CompleteTask { task } => {
                let today = clock::today();
                let q = task.trim().to_lowercase();
                let found = self.store.today(today).iter().find(|t| !t.done && t.title.to_lowercase().contains(&q)).map(|t| (t.id, t.title.clone()));
                match found {
                    Some((id, title)) if !q.is_empty() => {
                        self.store.toggle(id, today);
                        self.save();
                        answer(reply, format!("Marked done: {title}"));
                        self.refresh()
                    }
                    _ => {
                        answer(reply, format!("No open task today matches \"{task}\"."));
                        Ok(())
                    }
                }
            }
            McpCall::GetStatus => {
                answer(reply, self.status_text());
                Ok(())
            }
        }
    }

    /// Cards waiting for your answer for the agent via MCP.
    fn push_mcp_card(&mut self, agent: &str, tool: &str, action: String, reply: Sender<Reply>, ask: Ask, now: Instant) -> Result<()> {
        self.pending.push_back(Pending {
            session: format!("mcp:{agent}"),
            project: agent.to_string(),
            tool: tool.to_string(),
            text: action.clone(),
            action,
            reply,
            expires: now + Duration::from_secs(MCP_WAIT_SECS),
            wait: MCP_WAIT_SECS,
            ask,
            scroll: 0.0,
        });
        self.schedule_permission_timer();
        self.update()
    }

    /// User context for the agent (`get_status`).
    fn status_text(&self) -> String {
        const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        let now = clock::now();
        let iso = now.date().iso();
        let mut out = format!("Local time: {iso} {:02}:{:02} ({})\n", now.hour, now.minute, DAYS[now.weekday as usize % 7]);
        out.push_str(if self.locked_at.is_some() { "Screen: locked, the user is away\n" } else { "Screen: unlocked\n" });
        let focus = if self.pomodoro.is_active() {
            let left = self.pomodoro.remaining(Instant::now(), &self.settings.pomodoro).as_secs().div_ceil(60);
            let state = if self.pomodoro.is_paused() { "paused" } else { "running" };
            format!("{} {state}, about {left} min left", self.focus_phase_label())
        } else {
            "off".to_string()
        };
        out.push_str(&format!("Focus timer: {focus}\n"));
        let all = self.store.today(now.date());
        let open = all.iter().filter(|t| !t.done).count();
        out.push_str(&format!("Today's tasks: {open} open, {} done\n", all.len() - open));
        for t in all {
            let mut line = format!("- [{}] {}", if t.done { "x" } else { " " }, t.title);
            if let Some(hm) = &t.time {
                line.push_str(&format!(" (at {})", clock::time12(hm)));
            }
            if !t.done && t.due < iso {
                line.push_str(" (overdue)");
            }
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    fn answer(&mut self, decision: Decision) -> Result<()> {
        self.finish(Some(Reply { decision: Some(decision), ..Reply::default() }))
    }

    /// Closes the frontmost card. `None`: not answering, the question is returned to the agent UI.
    fn finish(&mut self, reply: Option<Reply>) -> Result<()> {
        if let Some(p) = self.pending.pop_front() {
            // The MCP agent session row is no longer waiting.
            if p.session.starts_with("mcp:") && p.project != "Claude Code" {
                let text = if reply.is_some() { "Got your answer" } else { "Waiting in the chat" };
                self.agents.mcp_activity(&p.project, text.to_string(), AgentState::Idle, Instant::now());
            }
            // A failed send means the bridge stopped waiting; nothing to do.
            if let Some(r) = reply {
                let _ = p.reply.send(r);
            }
        }
        self.ui.hover = None;
        self.schedule_permission_timer();
        self.update()
    }

    /// Picks option `i` on a question card. A plain question is answered immediately; multi-select
    /// marks the option and is answered via the Done button.
    fn choose(&mut self, i: usize) -> Result<()> {
        let Some(Pending { ask: Ask::Questions { list, index, answers, picked }, .. }) = self.pending.front_mut() else { return Ok(()) };
        let q = &list[*index];
        let Some(option) = q.options.get(i) else { return Ok(()) };
        if q.multi {
            match picked.iter().position(|&p| p == i) {
                Some(at) => {
                    picked.remove(at);
                }
                None => picked.push(i),
            }
            return self.refresh();
        }
        answers.insert(q.question.clone(), option.label.clone());
        self.next_question()
    }

    /// Done button on a multi-select question: marked labels are joined with commas.
    fn done_choosing(&mut self) -> Result<()> {
        let Some(Pending { ask: Ask::Questions { list, index, answers, picked }, .. }) = self.pending.front_mut() else { return Ok(()) };
        if picked.is_empty() {
            return Ok(());
        }
        let q = &list[*index];
        picked.sort_unstable();
        let labels: Vec<&str> = picked.iter().filter_map(|&i| q.options.get(i).map(|o| o.label.as_str())).collect();
        answers.insert(q.question.clone(), labels.join(", "));
        self.next_question()
    }

    /// Moves to the next question, or sends all answers if it was the last.
    fn next_question(&mut self) -> Result<()> {
        let Some(Pending { ask: Ask::Questions { list, index, answers, picked }, scroll, .. }) = self.pending.front_mut() else { return Ok(()) };
        *index += 1;
        picked.clear();
        *scroll = 0.0;
        if *index < list.len() {
            self.ui.hover = None;
            return self.refresh();
        }
        let answers = std::mem::take(answers);
        self.finish(Some(Reply { decision: Some(Decision::Allow), answers: Some(answers), ..Reply::default() }))
    }

    /// Scrolls the front card's content one mouse-wheel step, if the content is taller than the card.
    fn scroll_card(&mut self, down: bool) -> Result<()> {
        let max = (self.card_height() - self.shape.h).max(0.0);
        let Some(p) = self.pending.front_mut() else { return Ok(()) };
        let next = (p.scroll + if down { layout::CARD_STEP } else { -layout::CARD_STEP }).clamp(0.0, max);
        if next == p.scroll {
            return Ok(());
        }
        p.scroll = next;
        self.ui.hover = None;
        self.refresh()
    }

    /// Requests past their timeout are dropped; the agent has returned to its own dialog.
    fn expire_permissions(&mut self) -> Result<()> {
        let now = Instant::now();
        self.pending.retain(|p| p.expires > now);
        self.schedule_permission_timer();
        self.update()
    }

    fn schedule_permission_timer(&self) {
        match self.pending.iter().map(|p| p.expires).min() {
            Some(t) => set_timer(self.controller, T_PERMISSION, t.saturating_duration_since(Instant::now()).as_millis() as u32 + 50),
            None => kill_timer(self.controller, T_PERMISSION),
        }
    }

    /// Places the input window exactly over the field drawn in the panel.
    fn sync_input(&mut self, focus: bool) {
        if !self.ui.input_open || self.visual != Visual::Expanded {
            return;
        }
        if let Some(area) = self.painted.input {
            let r = self.island.to_screen(area);
            let compact = self.ui.tab == Tab::Command && self.input.text().is_empty();
            self.input.place(r, self.island.scale(), focus, compact);
        }
    }

    fn set_expanded(&mut self, on: bool) -> Result<()> {
        self.env.expanded = on;
        if on {
            self.env.glance = None;
            kill_timer(self.controller, T_GLANCE);
            self.chip = None;
            self.held_image = None;
            // The prayer reminder doesn't go away; it shows again when the panel closes.
            self.env.chip = self.prayer_now.is_some();
            kill_timer(self.controller, T_CHIP);
        } else {
            // Panel reopens on today, not the other day last viewed.
            self.ui.day = None;
            if self.ui.tab == Tab::Command {
                // Command bar is temporary; the next notch click opens Today.
                self.ui.tab = Tab::Today;
            }
        }
        self.apply(true)
    }

    fn toggle(&mut self) -> Result<()> {
        self.set_expanded(!self.env.expanded)
    }

    /// `mood`: Nochi during the glance, e.g. jumping for good news, turning for a reminder.
    fn show_glance(&mut self, text: &str, mood: Mood) -> Result<()> {
        if self.env.expanded || self.env.fullscreen {
            return Ok(());
        }
        self.glance_mood = mood;
        self.env.glance = Some(text.to_string());
        set_timer(self.controller, T_GLANCE, GLANCE_MS);
        self.apply(true)
    }

    // ---- Pomodoro --------------------------------------------------------------------------

    /// One one-shot timer: per second only when seconds are visible (hovered, panel open),
    /// per minute on the compact notch, and otherwise only when the phase ends. Paused: no timer.
    fn schedule_focus(&self) {
        let wake = match self.visual {
            Visual::Expanded => Wake::Second,
            Visual::Focus if self.env.hovering => Wake::Second,
            Visual::Focus => Wake::Minute,
            _ => Wake::End,
        };
        match self.pomodoro.next_wake(Instant::now(), wake) {
            // Slightly past the boundary, so text rounded up has already changed.
            Some(d) => set_timer(self.controller, T_FOCUS, d.as_millis() as u32 + 15),
            None => kill_timer(self.controller, T_FOCUS),
        }
    }

    /// Start, pause, or resume.
    fn focus_toggle(&mut self) -> Result<()> {
        self.pomodoro.toggle(Instant::now(), &self.settings.pomodoro);
        self.focus_changed()
    }

    fn focus_stop(&mut self) -> Result<()> {
        self.pomodoro.stop();
        self.focus_label = None;
        self.focus_changed()
    }

    /// Next task among today's open tasks; after the last one, no task.
    fn focus_next_task(&mut self) -> Result<()> {
        let open: Vec<u64> = self.store.today(clock::today()).iter().filter(|t| !t.done).map(|t| t.id).collect();
        let at = self.focus_task.and_then(|id| open.iter().position(|&o| o == id));
        self.focus_task = match at {
            Some(i) => open.get(i + 1).copied(),
            None => open.first().copied(),
        };
        self.refresh()
    }

    fn focus_changed(&mut self) -> Result<()> {
        self.env.focus = self.pomodoro.is_active();
        let before = self.visual.clone();
        self.apply(true)?;
        if self.visual == before {
            self.refresh()?;
        }
        self.schedule_focus();
        Ok(())
    }

    fn on_focus_timer(&mut self) -> Result<()> {
        let d = self.settings.pomodoro;
        match self.pomodoro.tick(Instant::now(), &d) {
            Some(f) => self.on_focus_finished(f),
            None => {
                self.refresh()?;
                self.schedule_focus();
                Ok(())
            }
        }
    }

    /// Pomodoro phase or timer finished: glance and chime.
    fn on_focus_finished(&mut self, finished: Finished) -> Result<()> {
        let d = self.settings.pomodoro;
        match finished {
            Finished::Focus { long } => {
                let rest = if long { d.long_break } else { d.short_break };
                let text = match self.focus_task_title() {
                    Some(t) => format!("Focus done \u{00B7} {t} \u{00B7} take {rest}"),
                    None => format!("Focus done \u{00B7} take {rest}"),
                };
                self.env.focus = true;
                self.play(Chime::Done);
                self.show_glance(&text, Mood::Happy { hop: true })?;
                self.focus_changed()
            }
            Finished::Timer => {
                let text = match self.focus_label.take() {
                    Some(l) => format!("Time's up \u{00B7} {l}"),
                    None => "Time's up".to_string(),
                };
                self.env.focus = false;
                self.play(Chime::Done);
                self.show_glance(&text, Mood::Happy { hop: true })?;
                self.focus_changed()
            }
            Finished::Break => {
                let next = if self.focus_key_ok { "Ctrl+Alt+P to focus" } else { "ready when you are" };
                self.focus_label = None;
                self.env.focus = false;
                self.play(Chime::Back);
                self.show_glance(&format!("Break over \u{00B7} {next}"), Mood::Curious)?;
                self.focus_changed()
            }
        }
    }

    // ---- Tasks -----------------------------------------------------------------------------

    fn save(&mut self) {
        // History past its limit is also dropped on every save.
        self.store.prune(clock::today());
        if let Err(e) = self.store.save() {
            log!("failed to save tasks: {e}");
        }
    }

    /// Adds tasks from one sentence; returns (task count, how many of them are for tomorrow).
    fn add_from_text(&mut self, text: &str) -> (usize, usize) {
        let parsed = parse(text);
        let today = clock::today();
        for p in &parsed {
            self.store.add(&p.title, if p.tomorrow { today.next() } else { today }, p.time);
        }
        if !parsed.is_empty() {
            self.save();
        }
        (parsed.len(), parsed.iter().filter(|p| p.tomorrow).count())
    }

    fn open_input(&mut self) -> Result<()> {
        self.ui.tab = Tab::Today;
        // A new task is always for today, so show today.
        self.ui.day = None;
        self.ui.input_open = true;
        self.ui.preview = parse(&self.input.text());
        if self.env.expanded {
            self.refresh()?;
            self.sync_input(true);
            Ok(())
        } else {
            self.ui.focus_pending = true;
            self.set_expanded(true)
        }
    }

    /// Text already typed is left in the field as a draft for the next open.
    fn close_input(&mut self) {
        self.ui.input_open = false;
        self.ui.notice = None;
        self.ui.focus_pending = false;
        self.ui.preview.clear();
        self.input.hide();
    }

    fn submit_input(&mut self) -> Result<()> {
        let (added, tomorrow) = self.add_from_text(&self.input.text());
        if added > 0 {
            self.ui.scroll = usize::MAX; // show the end of the list, where the new task usually appears
            self.input.clear();
            self.ui.preview.clear();
            // Tasks for tomorrow do not appear in "Today", so tell where they went.
            self.ui.notice = Some(match tomorrow {
                0 => format!("{} added", tasks(added)),
                n if n == added => format!("{} saved for tomorrow", tasks(n)),
                n => format!("{} added, {n} for tomorrow", tasks(added - n)),
            });
        }
        self.refresh()
    }

    // ---- Command bar -----------------------------------------------------------------------

    /// Ctrl+Alt+N: open the command bar, or close it if it is open.
    fn toggle_command(&mut self) -> Result<()> {
        if self.env.expanded && self.ui.tab == Tab::Command {
            self.set_expanded(false)
        } else {
            self.open_command()
        }
    }

    fn open_command(&mut self) -> Result<()> {
        self.index.refresh();
        self.ui.tab = Tab::Command;
        self.ui.cmd_selected = 0;
        self.ui.cmd_project = 0;
        self.ui.hover = None;
        self.ui.preview.clear();
        self.ui.notice = None;
        self.input.clear();
        self.ui.input_open = true;
        if self.env.expanded {
            self.refresh()?;
            self.sync_input(true);
            Ok(())
        } else {
            self.ui.focus_pending = true;
            self.set_expanded(true)
        }
    }

    fn close_command(&mut self) -> Result<()> {
        self.input.clear();
        self.set_expanded(false)
    }

    /// Latest agent project folders first.
    fn remember_project(&mut self, dir: &str) {
        if self.settings.projects.first().map(String::as_str) == Some(dir) {
            return;
        }
        self.settings.projects.retain(|p| p != dir);
        self.settings.projects.insert(0, dir.to_string());
        self.settings.projects.truncate(MAX_PROJECTS);
        self.save_settings();
    }

    fn projects(&self) -> Vec<Entry> {
        self.settings.projects.iter().map(|d| Entry { kind: Kind::Project, name: command::folder_name(d), target: d.clone() }).collect()
    }

    fn entry_row(&self, e: &Entry) -> CmdRow {
        let (icon, side, title) = match e.kind {
            Kind::App => (Icon::Today, "App", e.name.clone()),
            Kind::Folder => (Icon::Folder, "Folder", e.name.clone()),
            Kind::Project => (Icon::Code, "Project", format!("{} in VS Code", e.name)),
        };
        let target = deck_target(e);
        let pinned = self.deck.buttons.iter().any(|b| b.kind == deck::Kind::Open && b.value == target);
        CmdRow { section: None, icon, letter: (e.kind == Kind::App).then(|| paint::initial(&e.name)), title, side: side.into(), pinned: Some(pinned) }
    }

    /// Command bar content for the text in the field, with the action of each choice (same order: cards first).
    fn command_items(&self) -> (CommandView, Vec<CmdAction>) {
        let text = self.input.text();
        let q = text.trim();
        let mut card = None;
        let mut rows = Vec::new();
        let mut actions = Vec::new();
        let mut hints = vec![("\u{21B5}", "Run"), ("\u{2191}\u{2193}", "Move"), ("Esc", "Close")];
        let row = |icon, title: String, side: &str| CmdRow { section: None, icon, letter: None, title, side: side.to_string(), pinned: None };
        let web = |q: &str| (row(Icon::Globe, format!("Search the web for \u{201C}{q}\u{201D}"), "Browser"), CmdAction::Web(q.to_string()));
        let projects = self.projects();
        if q.is_empty() {
            let open: Vec<&Task> = self.store.today(clock::today()).into_iter().filter(|t| !t.done).collect();
            if self.pomodoro.is_paused() {
                let title = self.focus_task_title().map_or("Resume focus".to_string(), |t| format!("Resume focus \u{00B7} {t}"));
                rows.push(row(Icon::Timer, title, &pomodoro::minutes(self.pomodoro.remaining(Instant::now(), &self.settings.pomodoro))));
                actions.push(CmdAction::ResumeFocus);
            } else if let (false, Some(t)) = (self.pomodoro.is_active(), open.first()) {
                rows.push(row(Icon::Timer, format!("Start focus \u{00B7} {}", t.title), &format!("{} min", self.settings.pomodoro.focus)));
                actions.push(CmdAction::StartFocus(t.id));
            }
            if let Some(p) = projects.first() {
                let session = self.agents.sessions().into_iter().find(|s| s.dir.as_deref() == Some(p.target.as_str()));
                let mut r = self.entry_row(p);
                r.title = format!("Open {} in VS Code", p.name);
                r.side = session.map_or("Recent project".into(), |s| format!("Claude, {}", ago(s.updated.elapsed().as_secs() / 60)));
                rows.push(r);
                actions.push(CmdAction::Open(p.clone()));
            }
            let title = if open.is_empty() { "Nothing left today".to_string() } else { format!("{} left today", tasks(open.len())) };
            rows.push(row(Icon::Today, title, "Today"));
            actions.push(CmdAction::ShowToday);
            if let Some(first) = rows.first_mut() {
                first.section = Some("Suggested");
            }
        } else {
            match command::intent(q) {
                Intent::Timer { secs, label } => {
                    let detail = if label.is_empty() { format!("Ends {}", ends_at(secs)) } else { format!("{label} \u{00B7} ends {}", ends_at(secs)) };
                    card = Some(CmdCard::Timer { focus: true, time: command::clock(secs), detail });
                    actions.push(CmdAction::Timer { secs, label: label.clone(), focus: true });
                    rows.push(row(Icon::Timer, format!("Plain timer, {}", duration_words(secs)), "No focus mode"));
                    actions.push(CmdAction::Timer { secs, label: label.clone(), focus: false });
                    if !label.is_empty() {
                        rows.push(row(Icon::Today, format!("Add task \u{201C}{label}\u{201D}"), "Today"));
                        actions.push(CmdAction::AddTasks(label));
                    }
                    hints = vec![("\u{21B5}", "Start"), ("\u{2191}\u{2193}", "Move")];
                }
                Intent::Calc { value, expr } => {
                    card = Some(CmdCard::Calc { value: command::format_number(value), expr });
                    actions.push(CmdAction::Copy(command::plain_number(value)));
                    hints = vec![("\u{21B5}", "Copy result"), ("Esc", "Close")];
                }
                Intent::Claude { prompt } => {
                    let (name, dir) = match projects.get(self.ui.cmd_project % projects.len().max(1)) {
                        Some(p) => (p.name.clone(), p.target.clone()),
                        None => ("Home".to_string(), home_dir()),
                    };
                    card = Some(CmdCard::Claude { prompt: prompt.clone(), project: name, can_switch: projects.len() > 1 });
                    actions.push(CmdAction::Claude { prompt, dir });
                    hints = if projects.len() > 1 { vec![("\u{21B5}", "Send"), ("Tab", "Change project")] } else { vec![("\u{21B5}", "Send"), ("Esc", "Close")] };
                }
                Intent::Web { query } => {
                    let (r, a) = web(&query);
                    rows.push(r);
                    actions.push(a);
                }
                Intent::City { query } => {
                    for p in prayer::search(&query).into_iter().take(4) {
                        rows.push(row(Icon::Moon, format!("Prayer times for {}", p.name), "City"));
                        actions.push(CmdAction::SetCity(p));
                    }
                    rows.push(row(Icon::Pin, "Use my location".into(), "Windows Location"));
                    actions.push(CmdAction::Locate);
                    hints = vec![("\u{21B5}", "Choose"), ("\u{2191}\u{2193}", "Move"), ("Esc", "Close")];
                }
                Intent::Text => {
                    let hits = self.index.search(q, &projects, 3);
                    let parsed = parse(q);
                    let title = parsed.first().map_or_else(|| q.to_string(), |p| p.title.clone());
                    if hits.first().is_some_and(|h| h.0 >= 55) {
                        // Similar to an app or folder name: the result first, tasks after.
                        for (_, e) in &hits {
                            rows.push(self.entry_row(e));
                            actions.push(CmdAction::Open(e.clone()));
                        }
                        rows.push(row(Icon::Today, format!("Add task \u{201C}{title}\u{201D}"), "Today"));
                        actions.push(CmdAction::AddTasks(q.to_string()));
                    } else {
                        let mut chips = vec![parsed.first().and_then(|p| preview_of(p).chip).unwrap_or_else(|| "Today".into())];
                        if parsed.len() > 1 {
                            chips.push(format!("+{} more", parsed.len() - 1));
                        }
                        card = Some(CmdCard::Task { title, chips });
                        actions.push(CmdAction::AddTasks(q.to_string()));
                        for (_, e) in hits.iter().take(2) {
                            rows.push(self.entry_row(e));
                            actions.push(CmdAction::Open(e.clone()));
                        }
                    }
                    let (r, a) = web(q);
                    rows.push(r);
                    actions.push(a);
                }
            }
        }
        let deck = q.is_empty().then(|| self.deck_grid(0, 10));
        if let Some(g) = &deck {
            // A two-row deck only fits one suggestion.
            let keep = if layout::deck_rows(g, layout::CMD_DECK_ROWS) >= 2 { 1 } else { 2 };
            rows.truncate(keep);
            actions.truncate(keep);
        }
        let selected = self.ui.cmd_selected.min(actions.len().saturating_sub(1));
        let view = CommandView { empty: q.is_empty(), card, rows, deck, selected, hints };
        (view, actions)
    }

    fn run_cmd(&mut self, i: usize) -> Result<()> {
        let (_, actions) = self.command_items();
        let Some(action) = actions.get(i).cloned() else { return Ok(()) };
        let mut glance = None;
        match action {
            CmdAction::Timer { secs, label, focus } => {
                self.close_command()?;
                return self.start_timer(secs, label, focus);
            }
            CmdAction::Copy(text) => {
                self.own_clip_at = Some(Instant::now());
                if launcher::copy_text(self.controller, &text) {
                    glance = Some(format!("Copied \u{00B7} {text}"));
                }
            }
            CmdAction::Claude { prompt, dir } => {
                if !launcher::start_claude(&dir, &prompt) {
                    glance = Some("Couldn't start Claude Code".to_string());
                }
            }
            CmdAction::AddTasks(text) => {
                let (n, tomorrow) = self.add_from_text(&text);
                glance = Some(if n > 0 && n == tomorrow { format!("{} saved for tomorrow", tasks(n)) } else { format!("{} added", tasks(n)) });
            }
            CmdAction::Open(e) => open_entry(&e),
            CmdAction::Web(query) => {
                launcher::web_search(&query);
            }
            CmdAction::ResumeFocus => {
                self.close_command()?;
                return self.focus_toggle();
            }
            CmdAction::StartFocus(id) => {
                self.close_command()?;
                return self.start_focus(Some(id));
            }
            CmdAction::SetCity(place) => return self.set_prayer_place(place),
            CmdAction::Locate => return self.on_widget(WidgetCmd::Locate),
            CmdAction::ShowToday => {
                self.ui.tab = Tab::Today;
                self.close_input();
                self.ui.hover = None;
                return self.refresh();
            }
        }
        self.close_command()?;
        match glance {
            Some(text) => self.show_glance(&text, Mood::Happy { hop: true }),
            None => Ok(()),
        }
    }

    /// Up/down arrows move the selection; Tab switches the project for "claude …".
    fn on_input_key(&mut self, vk: u16) -> Result<()> {
        if self.ui.tab != Tab::Command || !self.env.expanded {
            return Ok(());
        }
        let (view, actions) = self.command_items();
        match VIRTUAL_KEY(vk) {
            VK_UP => self.ui.cmd_selected = view.selected.saturating_sub(1),
            VK_DOWN => self.ui.cmd_selected = (view.selected + 1).min(actions.len().saturating_sub(1)),
            VK_TAB if matches!(view.card, Some(CmdCard::Claude { can_switch: true, .. })) => self.ui.cmd_project += 1,
            _ => return Ok(()),
        }
        self.refresh()
    }

    /// Pin button on result row `i`: adds that result to the deck, or removes it.
    fn toggle_pin(&mut self, i: usize) -> Result<()> {
        let (view, actions) = self.command_items();
        let Some(CmdAction::Open(e)) = actions.get(view.card.is_some() as usize + i).cloned() else { return Ok(()) };
        let target = deck_target(&e);
        match self.deck.buttons.iter().position(|b| b.kind == deck::Kind::Open && b.value == target) {
            Some(at) => {
                self.deck.buttons.remove(at);
            }
            None => {
                let glyph = match e.kind {
                    Kind::App => Glyph::App,
                    Kind::Folder => Glyph::Folder,
                    Kind::Project => Glyph::Code,
                };
                self.deck.buttons.push(Button { label: e.name.clone(), glyph, value: target, ..Button::new(deck::Kind::Open) });
            }
        }
        self.save_deck();
        self.refresh()
    }

    /// Focus session with the length from Settings, for a given task.
    fn start_focus(&mut self, task: Option<u64>) -> Result<()> {
        self.focus_task = task;
        self.focus_label = None;
        let length = self.settings.pomodoro.of(Phase::Focus);
        self.pomodoro.start(Instant::now(), length, true);
        self.focus_changed()
    }

    /// "25m review PR": a focus session (or plain timer) of that length. A label matching an open task
    /// links that task.
    fn start_timer(&mut self, secs: u64, label: String, focus: bool) -> Result<()> {
        let task = self.store.today(clock::today()).iter().find(|t| !t.done && t.title.eq_ignore_ascii_case(&label)).map(|t| t.id);
        self.focus_task = if focus { task } else { None };
        self.focus_label = (task.is_none() || !focus).then_some(label).filter(|l| !l.is_empty());
        self.pomodoro.start(Instant::now(), Duration::from_secs(secs), focus);
        self.focus_changed()
    }

    // ---- Deck ------------------------------------------------------------------------------

    /// App icon for Open buttons, fetched once per target.
    fn deck_icon(&self, b: &Button) -> Option<Thumb> {
        if b.kind != deck::Kind::Open || b.value.contains("://") || b.value.trim().is_empty() {
            return None;
        }
        self.deck_icons.borrow_mut().entry(b.value.clone()).or_insert_with(|| launcher::app_icon(&b.value, 32)).clone()
    }

    fn deck_key(&self, i: usize, b: &Button) -> DeckKey {
        DeckKey {
            label: b.title(),
            glyph: paint::glyph_icon(b.glyph),
            image: self.deck_icon(b),
            number: (i < 10).then_some(((i + 1) % 10) as u8),
            on: b.on,
        }
    }

    /// `count` keys starting at `first`; the Add box is included if the last key is here.
    fn deck_grid(&self, first: usize, count: usize) -> DeckGrid {
        let keys: Vec<DeckKey> = self.deck.buttons.iter().enumerate().skip(first).take(count).map(|(i, b)| self.deck_key(i, b)).collect();
        let add = first + keys.len() == self.deck.buttons.len() && keys.len() < count;
        DeckGrid { keys, first, add }
    }

    fn deck_pages(&self) -> usize {
        (self.deck.buttons.len() + 1).div_ceil(layout::DECK_PAGE).max(1)
    }

    fn deck_page(&self) -> DeckPage {
        let pages = self.deck_pages();
        let page = self.ui.deck_page.min(pages - 1);
        DeckPage { grid: self.deck_grid(page * layout::DECK_PAGE, layout::DECK_PAGE), page, pages }
    }

    fn editor_view(&self) -> Option<DeckEditor> {
        let e = self.ui.editor.as_ref()?;
        let b = &e.draft;
        let field = |id: u8, caption: &'static str, value: &str, placeholder: &'static str| EditField {
            id,
            caption,
            value: value.to_string(),
            placeholder,
            active: e.field == Some(id),
            keycaps: None,
        };
        let mut fields = Vec::new();
        match b.kind {
            deck::Kind::Open => fields.push(field(1, "Open", &b.value, "Path, app or https:// link")),
            deck::Kind::Keys => {
                let mut f = field(1, "Keys", &b.value, "Ctrl+Shift+M");
                f.keycaps = deck::parse_keys(&b.value).map(|k| deck::format_keys(&k).split('+').map(str::to_string).collect());
                fields.push(f);
            }
            deck::Kind::Command => {
                fields.push(field(1, "Command", &b.value, "npm run dev"));
                fields.push(field(2, "Folder", &b.dir, "Home folder"));
            }
            deck::Kind::Text => fields.push(field(1, "Text", &b.value, "Text to type, \\n for a new line")),
            deck::Kind::Notch => match b.notch {
                deck::NotchKind::Focus => fields.push(field(1, "Minutes", &b.value, "50")),
                deck::NotchKind::Claude => fields.push(field(1, "Prompt", &b.value, "Review the open PR")),
                deck::NotchKind::Lock => {}
            },
        }
        let index = e.index.unwrap_or(self.deck.buttons.len());
        Some(DeckEditor {
            title: e.index.map_or("New button".to_string(), |i| format!("Button {}", i + 1)),
            glyph: paint::glyph_icon(b.glyph),
            image: self.deck_icon(b),
            label: field(0, "Label", &b.label, "Label"),
            kind: b.kind,
            notch: b.notch,
            fields,
            recording: e.recording,
            toggle: b.toggle,
            hotkey: (index < 10).then(|| format!("Ctrl+Alt+{}", (index + 1) % 10)),
            can_delete: e.index.is_some(),
            error: e.error.map(str::to_string),
        })
    }

    /// Ctrl+Alt+1..0 for the first ten keys; only those with a key are registered.
    fn sync_deck_hotkeys(&mut self) {
        let want = self.deck.buttons.len().min(10);
        unsafe {
            for i in 0..10 {
                let _ = UnregisterHotKey(Some(self.controller), HOTKEY_DECK + i);
            }
            for i in 0..want {
                let key = if i == 9 { '0' } else { (b'1' + i as u8) as char };
                if RegisterHotKey(Some(self.controller), HOTKEY_DECK + i as i32, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, key as u32).is_err() {
                    log!("Ctrl+Alt+{key} is taken by another app");
                }
            }
        }
    }

    fn save_deck(&mut self) {
        if let Err(e) = self.deck.save() {
            log!("failed to save deck: {e}");
        }
        self.sync_deck_hotkeys();
    }

    /// Runs key `i`. From the panel, the notch closes first and returns focus to the app that
    /// was in use; from a hotkey, the key is sent after Ctrl+Alt is released.
    fn run_deck(&mut self, i: usize) -> Result<()> {
        let Some(b) = self.deck.buttons.get(i).cloned() else { return Ok(()) };
        if let Some(problem) = b.problem() {
            return self.show_glance(&format!("{} \u{00B7} {problem}", b.title()), Mood::Curious);
        }
        let from_panel = self.env.expanded;
        if from_panel {
            self.input.clear();
            self.set_expanded(false)?;
        }
        if b.toggle {
            self.deck.buttons[i].on = !b.on;
        }
        let send = |what: Send| PendingSend { what, label: b.title(), tries: 0 };
        let result = match b.kind {
            deck::Kind::Open => {
                launcher::open(b.value.trim());
                Ok(())
            }
            deck::Kind::Keys | deck::Kind::Text => {
                if from_panel && !self.last_app.is_invalid() {
                    launcher::focus_window(self.last_app);
                }
                let what = match b.kind {
                    deck::Kind::Keys => Send::Keys(deck::parse_keys(&b.value).unwrap_or_default()),
                    _ => Send::Text(deck::text_to_type(&b.value)),
                };
                self.sending = Some(send(what));
                // Short delay so focus actually moves before the key is sent.
                set_timer(self.controller, T_SEND, if from_panel { 120 } else { 30 });
                Ok(())
            }
            deck::Kind::Command => {
                launcher::run_command(&b.dir, &b.value);
                self.show_glance(&format!("Started {} \u{00B7} {}", b.title(), b.value.trim()), Mood::Happy { hop: false })
            }
            deck::Kind::Notch => match b.notch {
                deck::NotchKind::Focus => {
                    let minutes = b.value.trim().parse::<u64>().unwrap_or(25);
                    self.start_timer(minutes * 60, String::new(), true)
                }
                deck::NotchKind::Claude => {
                    let dir = self.settings.projects.first().cloned().unwrap_or_else(home_dir);
                    launcher::start_claude(&dir, b.value.trim());
                    Ok(())
                }
                deck::NotchKind::Lock => {
                    launcher::lock_screen();
                    Ok(())
                }
            },
        };
        self.refresh()?;
        result
    }

    /// Sends the pending key or text, after the hotkey's modifiers are released.
    fn on_send_timer(&mut self) -> Result<()> {
        let Some(mut s) = self.sending.take() else { return Ok(()) };
        if launcher::modifiers_down() && s.tries < 40 {
            s.tries += 1;
            self.sending = Some(s);
            set_timer(self.controller, T_SEND, 25);
            return Ok(());
        }
        let ok = match &s.what {
            Send::Keys(keys) => launcher::send_keys(keys),
            Send::Text(text) => launcher::type_text(text),
        };
        // App name from the window title: the last part after " - " is usually the app name.
        let title = launcher::window_title(watch::foreground());
        let app: String = title.rsplit(" - ").next().unwrap_or(&title).chars().take(32).collect();
        let text = match (&s.what, ok) {
            (_, false) => format!("{} \u{00B7} couldn't send", s.label),
            (Send::Keys(keys), true) => format!("Sent {} \u{00B7} {app}", deck::format_keys(keys)),
            (Send::Text(_), true) => format!("Typed {} \u{00B7} {app}", s.label),
        };
        self.show_glance(&text, Mood::Curious)
    }

    fn open_editor(&mut self, index: Option<usize>) -> Result<()> {
        let draft = index.and_then(|i| self.deck.buttons.get(i).cloned()).unwrap_or_else(|| Button::new(deck::Kind::Open));
        self.close_input();
        self.input.clear();
        self.ui.tab = Tab::Deck;
        self.ui.editor = Some(Editor { index, draft, field: None, recording: false, error: None });
        self.ui.hover = None;
        if self.env.expanded {
            self.refresh()
        } else {
            self.set_expanded(true)
        }
    }

    fn close_editor(&mut self) -> Result<()> {
        self.ui.editor = None;
        RECORDING.store(false, Ordering::Relaxed);
        self.close_input();
        self.input.clear();
        self.ui.hover = None;
        self.refresh()
    }

    /// Text in the input field goes into the field being typed.
    fn commit_field(&mut self) {
        let text = self.input.text();
        if let Some(e) = &mut self.ui.editor {
            match e.field {
                Some(0) => e.draft.label = text,
                Some(1) => e.draft.value = text,
                Some(2) => e.draft.dir = text,
                _ => {}
            }
        }
    }

    fn edit_field(&mut self, id: u8) -> Result<()> {
        self.commit_field();
        let Some(e) = &mut self.ui.editor else { return Ok(()) };
        e.field = Some(id);
        e.error = None;
        let value = match id {
            0 => e.draft.label.clone(),
            1 => e.draft.value.clone(),
            _ => e.draft.dir.clone(),
        };
        self.input.set_text(&value);
        self.ui.input_open = true;
        self.refresh()?;
        self.sync_input(true);
        Ok(())
    }

    fn on_edit(&mut self, cmd: EditCmd) -> Result<()> {
        match cmd {
            EditCmd::Back => return self.close_editor(),
            EditCmd::Field(id) => return self.edit_field(id),
            EditCmd::Record => {
                self.edit_field(1)?;
                if let Some(e) = &mut self.ui.editor {
                    e.recording = true;
                }
                RECORDING.store(true, Ordering::Relaxed);
                return self.refresh();
            }
            _ => {}
        }
        self.commit_field();
        RECORDING.store(false, Ordering::Relaxed);
        self.close_input();
        let Some(e) = &mut self.ui.editor else { return Ok(()) };
        e.field = None;
        e.recording = false;
        e.error = None;
        let b = &mut e.draft;
        match cmd {
            EditCmd::Glyph => b.glyph = b.glyph.next(),
            EditCmd::Kind(k) if k != b.kind => {
                let default_glyph = Button::new(b.kind).glyph == b.glyph;
                b.kind = k;
                b.value.clear();
                b.dir.clear();
                if default_glyph {
                    b.glyph = Button::new(k).glyph;
                }
            }
            EditCmd::Notch(n) if n != b.notch => {
                b.notch = n;
                b.value.clear();
                b.glyph = match n {
                    deck::NotchKind::Focus => Glyph::Timer,
                    deck::NotchKind::Claude => Glyph::Sparkle,
                    deck::NotchKind::Lock => Glyph::Lock,
                };
            }
            EditCmd::State => b.toggle = !b.toggle,
            EditCmd::Save => {
                if let Some(problem) = b.problem() {
                    e.error = Some(problem);
                    return self.refresh();
                }
                let (index, draft) = (e.index, b.clone());
                self.deck_icons.borrow_mut().remove(&draft.value);
                match index {
                    Some(i) if i < self.deck.buttons.len() => self.deck.buttons[i] = draft,
                    _ => self.deck.buttons.push(draft),
                }
                self.save_deck();
                return self.close_editor();
            }
            EditCmd::Delete => {
                if let Some(i) = e.index.filter(|i| *i < self.deck.buttons.len()) {
                    self.deck.buttons.remove(i);
                    self.save_deck();
                }
                return self.close_editor();
            }
            _ => {}
        }
        self.refresh()
    }

    /// Combination recorded in the Keys field.
    fn on_recorded(&mut self, vk: u16, mods: usize) -> Result<()> {
        RECORDING.store(false, Ordering::Relaxed);
        let combo = deck::recorded(mods & 1 != 0, mods & 2 != 0, mods & 4 != 0, mods & 8 != 0, vk);
        let Some(e) = &mut self.ui.editor else { return Ok(()) };
        e.recording = false;
        e.draft.value = combo.clone();
        self.input.set_text(&combo);
        self.refresh()
    }

    /// Something is dragged over the notch: the notch widens and shows what will be added.
    fn on_drag_enter(&mut self, items: &[Dropped]) -> Result<()> {
        if self.env.expanded {
            return Ok(());
        }
        let name = match items {
            [one] => dropped_button(one).title(),
            many => format!("{} items", many.len()),
        };
        self.show_chip(SmartChip { lead: Lead::Icon(Icon::Grid), kicker: "Deck".into(), warn: false, title: format!("Drop to add {name}"), buttons: Vec::new(), mood: Mood::Curious })?;
        // Stays shown while still being dragged.
        kill_timer(self.controller, T_CHIP);
        self.drop_chip = self.chip.is_some();
        Ok(())
    }

    fn on_drag_leave(&mut self) -> Result<()> {
        if std::mem::take(&mut self.drop_chip) {
            return self.dismiss_chip();
        }
        Ok(())
    }

    /// Dropped on the notch: each item becomes an Open key in the deck (existing ones are skipped).
    fn on_drop(&mut self, items: Vec<Dropped>) -> Result<()> {
        if std::mem::take(&mut self.drop_chip) {
            self.dismiss_chip()?;
        }
        let mut added = Vec::new();
        for d in &items {
            let b = dropped_button(d);
            if !self.deck.buttons.iter().any(|x| x.kind == deck::Kind::Open && x.value == b.value) {
                added.push(b.title());
                self.deck.buttons.push(b);
            }
        }
        if added.is_empty() {
            return self.show_glance("Already in your Deck", Mood::Curious);
        }
        self.save_deck();
        if self.env.expanded {
            return self.refresh();
        }
        let n = self.deck.buttons.len();
        let text = match added.as_slice() {
            [one] if n <= 10 => format!("Added {one} \u{00B7} Ctrl+Alt+{}", n % 10),
            [one] => format!("Added {one} \u{00B7} in the Deck tab"),
            many => format!("{} buttons added \u{00B7} Deck", many.len()),
        };
        self.show_glance(&text, Mood::Happy { hop: true })
    }

    /// Notch is pulled: stays attached to the top edge and only moves sideways, initially reluctant
    /// (lags behind while shaking its head, Nochi squints in annoyance), then gives in and follows. On release it
    /// bounces to the top center of the monitor under the cursor and stays there.
    fn on_move(&mut self, phase: usize, lp: isize) -> Result<()> {
        let cursor = island::unpack(lp);
        // Top edge of the monitor under the cursor: the notch changes edge as soon as the cursor enters another monitor.
        let monitor = display::at(cursor.0, cursor.1);
        let top = display::bounds(monitor).map_or(self.placement().monitor.y, |b| b.y);
        match phase {
            island::MOVE_START => {
                self.move_from = Some(self.monitor);
                self.floating = true;
                self.resisting = true;
                self.glide = None;
                kill_timer(self.controller, T_GLIDE);
                self.island.begin_float(cursor)?;
                self.refresh()
            }
            island::MOVE_MOVE if self.floating => {
                let resisting = self.island.float_to(cursor, top)?;
                if resisting != self.resisting {
                    self.resisting = resisting;
                    self.refresh()?;
                }
                Ok(())
            }
            island::MOVE_END if self.floating => {
                self.island.float_to(cursor, top)?;
                if display::bounds(monitor).is_some() {
                    self.monitor = monitor;
                }
                // Slides from the current position to the target monitor's canvas.
                let to = self.placement().canvas();
                self.glide = Some(Glide { from: self.island.window_pos(), to: (to.x, to.y), started: Instant::now() });
                set_timer(self.controller, T_GLIDE, 15);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// One slide step; at the end, the notch attaches again and the target monitor's layout is applied.
    fn on_glide(&mut self) -> Result<()> {
        let Some(Glide { from, to, started }) = self.glide else { return Ok(()) };
        let t = (started.elapsed().as_secs_f32() * 1000.0 / GLIDE_MS).min(1.0);
        // Returns to its place with a slight overshoot then bounce (easeOutBack).
        let (c1, u) = (1.4, t - 1.0);
        let e = 1.0 + (c1 + 1.0) * u.powi(3) + c1 * u.powi(2);
        let lerp = |a: i32, b: i32| a + ((b - a) as f32 * e).round() as i32;
        self.island.move_window(lerp(from.0, to.0), lerp(from.1, to.1));
        if t < 1.0 {
            set_timer(self.controller, T_GLIDE, 15);
            return Ok(());
        }
        self.glide = None;
        self.floating = false;
        self.resisting = false;
        self.island.settle_at(self.placement().canvas());
        self.island.end_float()?;
        // Layout (and window region) is applied again, for the target monitor if it changed.
        self.relayout()?;
        let moved = self.move_from.take().is_some_and(|from| from != self.monitor);
        self.settings.monitor = MonitorMode::Stay;
        self.settings.display = display::device_name(self.monitor);
        self.save_settings();
        if moved {
            return self.show_glance(&format!("Notch stays on {}", display::label(self.monitor)), Mood::Happy { hop: true });
        }
        self.refresh()
    }

    /// Right-click: edit the deck key.
    fn on_right_click(&mut self, lp: isize) -> Result<()> {
        if !self.env.expanded {
            return Ok(());
        }
        let (x, y) = self.island.to_canvas(lp);
        match self.painted.hit_at(x, y) {
            Some(Hit::DeckKey(i)) => self.open_editor(Some(i)),
            Some(Hit::DeckAdd) => self.open_editor(None),
            _ => Ok(()),
        }
    }

    // ---- Smart chips -----------------------------------------------------------------------

    fn show_chip(&mut self, chip: SmartChip) -> Result<()> {
        if self.env.expanded || self.env.fullscreen || self.env.permission {
            return Ok(());
        }
        self.chip = Some(chip);
        self.held_image = None;
        self.env.chip = true;
        self.env.glance = None;
        kill_timer(self.controller, T_GLANCE);
        self.ui.hover = None;
        set_timer(self.controller, T_CHIP, CHIP_MS);
        // A new chip is always redrawn, even if another chip is showing.
        if self.visual == Visual::Chip {
            self.refresh()
        } else {
            self.apply(true)
        }
    }

    /// Dismisses a regular chip; the prayer reminder waiting beneath it shows again.
    fn dismiss_chip(&mut self) -> Result<()> {
        kill_timer(self.controller, T_CHIP);
        self.held_image = None;
        if self.chip.take().is_none() && !self.env.chip {
            return Ok(());
        }
        self.env.chip = self.prayer_now.is_some();
        self.apply(true)
    }

    /// Chip to show: a regular chip first, then the prayer reminder.
    fn shown_chip(&self) -> Option<&SmartChip> {
        self.chip.as_ref().or(self.prayer_now.as_ref().map(|p| &p.chip))
    }

    /// Reminder for prayer `i` (time `at`, local minutes): chip with a crescent moon that
    /// stays until Done is clicked, at most `prayer::REMINDER_HOLD_MIN`. A regular chip that
    /// is showing gets replaced; an agent permission card still takes priority (`decide`), and a panel or fullscreen app
    /// only delays it.
    fn show_prayer(&mut self, i: usize, at: f64) -> Result<()> {
        let (kicker, title) = prayer::reminder_text(i, at, false);
        let done = (button(Some("Done"), Some(Icon::Check), ButtonStyle::Primary), ChipAction::PrayerDone);
        let chip = SmartChip { lead: Lead::Moon, kicker, warn: false, title, buttons: vec![done], mood: Mood::Happy { hop: false } };
        self.prayer_now = Some(PrayerNow { index: i, at, chip });
        set_timer(self.controller, T_PRAYER, prayer::REMINDER_HOLD_MIN * 60 * 1000);
        kill_timer(self.controller, T_CHIP);
        self.chip = None;
        self.held_image = None;
        self.env.chip = true;
        self.env.glance = None;
        kill_timer(self.controller, T_GLANCE);
        self.ui.hover = None;
        if self.visual == Visual::Chip {
            self.refresh()
        } else {
            self.apply(true)
        }
    }

    /// Dismisses the prayer reminder (Done, or time ran out) and stops the adhan if still playing.
    fn dismiss_prayer(&mut self) -> Result<()> {
        kill_timer(self.controller, T_PRAYER);
        if self.prayer_now.take().is_none() {
            return Ok(());
        }
        adhan::close();
        self.env.chip = self.chip.is_some();
        self.apply(true)
    }

    fn run_chip(&mut self, i: usize) -> Result<()> {
        let Some(action) = self.shown_chip().and_then(|c| c.buttons.get(i)).map(|(_, a)| a.clone()) else { return Ok(()) };
        let held = self.held_image.take();
        self.dismiss_chip()?;
        match action {
            ChipAction::StartFocus(task) => self.start_focus(task),
            ChipAction::OpenUrl(url) | ChipAction::Open(url) => {
                launcher::open(&url);
                Ok(())
            }
            ChipAction::AddTask(title) => {
                self.store.add(&title, clock::today(), None);
                self.save();
                self.show_glance("1 task added", Mood::Happy { hop: true })
            }
            ChipAction::CopyImage => {
                let copied = match &held {
                    Some(dib) => {
                        self.own_clip_at = Some(Instant::now());
                        chips::copy_image(self.controller, dib)
                    }
                    // Too large to hold: still safe if the clipboard has not changed.
                    None => chips::hold_image(self.controller).is_some(),
                };
                if copied {
                    self.show_glance("Image copied", Mood::Happy { hop: true })
                } else {
                    self.show_glance("Couldn't copy the image", Mood::Curious)
                }
            }
            ChipAction::SaveImage => {
                let now = clock::now();
                let stamp = format!("{} {:02}.{:02}.{:02}", now.date().iso(), now.hour, now.minute, now.second);
                match chips::save_image(self.controller, held.as_deref(), &stamp) {
                    Some(path) => {
                        let name = command::folder_name(&path.display().to_string());
                        self.show_glance(&format!("Saved \u{00B7} {name}"), Mood::Happy { hop: true })
                    }
                    None => self.show_glance("Couldn't save the image", Mood::Curious),
                }
            }
            ChipAction::Reveal(path) => {
                launcher::reveal(&path);
                Ok(())
            }
            ChipAction::ActivateClaude => {
                launcher::activate_claude();
                Ok(())
            }
            ChipAction::OpenProject(dir) => {
                launcher::open_project(&dir);
                Ok(())
            }
            ChipAction::PrayerDone => self.dismiss_prayer(),
        }
    }

    /// Greeting once a day (when the notch starts or the screen is unlocked), and "Welcome back" after
    /// a long screen lock.
    fn maybe_greet(&mut self, unlocked: bool) -> Result<()> {
        const AWAY: Duration = Duration::from_secs(60 * 60);
        let away = self.locked_at.take().map(|t| t.elapsed());
        if !self.settings.chips || self.env.expanded || self.env.fullscreen {
            return Ok(());
        }
        let now = clock::now();
        let iso = now.date().iso();
        let hello = if self.settings.greeted != iso {
            self.settings.greeted = iso;
            self.save_settings();
            greeting(now.hour)
        } else if unlocked && away.is_some_and(|d| d >= AWAY) {
            "Welcome back"
        } else {
            return Ok(());
        };
        let name = user_name();
        let kicker = if name.is_empty() { hello.to_string() } else { format!("{hello}, {name}") };
        let all = self.store.today(now.date());
        let open: Vec<&Task> = all.iter().copied().filter(|t| !t.done).collect();
        let next = open.iter().min_by_key(|t| t.time.clone().unwrap_or_else(|| "99:99".into()));
        let title = match next {
            Some(t) => match &t.time {
                Some(hm) => format!("{} \u{00B7} {} at {}", tasks(open.len()), t.title, clock::time12(hm)),
                None => format!("{} \u{00B7} {}", tasks(open.len()), t.title),
            },
            None if all.is_empty() => "Nothing planned today".to_string(),
            None => "All done for today".to_string(),
        };
        let mut buttons = Vec::new();
        if let (Some(t), false) = (next, self.pomodoro.is_active()) {
            buttons.push((button(Some("Focus"), Some(Icon::Timer), ButtonStyle::Amber), ChipAction::StartFocus(Some(t.id))));
        }
        self.show_chip(SmartChip { lead: Lead::Pet, kicker, warn: false, title, buttons, mood: Mood::Happy { hop: true } })
    }

    /// New link or image copied.
    fn on_clipboard(&mut self) -> Result<()> {
        // History is recorded even if the chip is off or the panel is open; copies from the notch itself are skipped
        // (their order is already set when copying).
        if self.settings.clip_history && !self.own_clip_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            if let Some(text) = chips::read_plain_text(self.controller) {
                self.remember_clip(text);
                if self.env.expanded && self.ui.tab == Tab::Widgets {
                    self.refresh()?;
                }
            }
        }
        if !self.settings.chips || self.env.expanded || self.env.fullscreen {
            return Ok(());
        }
        if self.own_clip_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return Ok(());
        }
        let Some(clip) = chips::read_clipboard(self.controller) else { return Ok(()) };
        if self.last_clip.as_ref().is_some_and(|(c, t)| *c == clip && t.elapsed() < Duration::from_secs(5)) {
            return Ok(());
        }
        self.last_clip = Some((clip.clone(), Instant::now()));
        let chip = match clip {
            Clip::Link { url, host, title } => SmartChip {
                lead: Lead::Pet,
                kicker: format!("Link copied \u{00B7} {host}"),
                warn: false,
                buttons: vec![
                    (button(Some("Open"), None, ButtonStyle::Plain), ChipAction::OpenUrl(url)),
                    (button(None, Some(Icon::Today), ButtonStyle::Plain), ChipAction::AddTask(format!("Read {title} ({host})"))),
                ],
                title,
                mood: Mood::Curious,
            },
            Clip::Image { w, h, thumb } => {
                let mut buttons = vec![
                    (button(Some("Copy"), Some(Icon::Copy), ButtonStyle::Primary), ChipAction::CopyImage),
                    (button(Some("Save"), None, ButtonStyle::Plain), ChipAction::SaveImage),
                ];
                if let Some(dir) = launcher::pictures_dir() {
                    buttons.push((button(None, Some(Icon::Folder), ButtonStyle::Plain), ChipAction::Open(dir.join("Screenshots").display().to_string())));
                }
                SmartChip { lead: Lead::Thumb(thumb), kicker: "Screenshot".into(), warn: false, title: format!("{w} \u{00D7} {h}"), buttons, mood: Mood::Curious }
            }
        };
        let image = matches!(chip.lead, Lead::Thumb(_));
        self.show_chip(chip)?;
        if image && self.chip.is_some() {
            self.held_image = chips::hold_image(self.controller);
        }
        Ok(())
    }

    fn on_download(&mut self, path: String) -> Result<()> {
        if !self.settings.chips {
            return Ok(());
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let buttons = vec![
            (button(Some("Open"), None, ButtonStyle::Primary), ChipAction::Open(path.clone())),
            (button(None, Some(Icon::Folder), ButtonStyle::Plain), ChipAction::Reveal(path.clone())),
        ];
        let title = command::folder_name(&path);
        self.show_chip(SmartChip { lead: Lead::Icon(Icon::Download), kicker: format!("Downloaded \u{00B7} {}", file_size(size)), warn: false, title, buttons, mood: Mood::Happy { hop: false } })
    }

    // ---- Events ----------------------------------------------------------------------------

    fn command(&mut self, cmd: &str) -> Result<()> {
        log!("command: {cmd}");
        match cmd {
            "toggle" => self.toggle(),
            "expand" => self.set_expanded(true),
            "collapse" => self.set_expanded(false),
            "input" => self.open_input(),
            "focus" => self.focus_toggle(),
            "focus-stop" => self.focus_stop(),
            "tab:today" | "tab:focus" | "tab:deck" | "tab:widgets" | "tab:settings" => {
                self.ui.tab = match cmd {
                    "tab:focus" => Tab::Focus,
                    "tab:settings" => Tab::Settings,
                    "tab:deck" => Tab::Deck,
                    "tab:widgets" if self.widgets_on() => Tab::Widgets,
                    _ => Tab::Today,
                };
                self.ui.editor = None;
                self.close_input();
                if self.env.expanded {
                    self.refresh()
                } else {
                    self.set_expanded(true)
                }
            }
            "agent" | "tab:agent" => {
                self.ui.tab = Tab::Agent;
                self.close_input();
                if self.env.expanded {
                    self.refresh()
                } else {
                    self.set_expanded(true)
                }
            }
            "deck" => {
                self.ui.tab = Tab::Deck;
                self.ui.editor = None;
                self.close_input();
                if self.env.expanded {
                    self.refresh()
                } else {
                    self.set_expanded(true)
                }
            }
            _ => {
                // Debug build only: open the editor or run deck key n, for testing without clicking.
                if let Some(n) = cmd.strip_prefix("deck-edit:").filter(|_| cfg!(debug_assertions)).and_then(|n| n.parse::<usize>().ok()) {
                    return self.open_editor(n.checked_sub(1));
                }
                // Debug build only: simulated drag "move:<start|move|end>:<x>,<y>" (screen pixels).
                if let Some(rest) = cmd.strip_prefix("move:").filter(|_| cfg!(debug_assertions)) {
                    let mut parts = rest.split([':', ',']);
                    let phase = match parts.next() {
                        Some("start") => island::MOVE_START,
                        Some("move") => island::MOVE_MOVE,
                        _ => island::MOVE_END,
                    };
                    let mut num = || parts.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                    let (x, y) = (num(), num());
                    return self.on_move(phase, ((y as i16 as u16 as isize) << 16) | x as i16 as u16 as isize);
                }
                // Debug builds only: answer the front card like a click, "card:allow|deny|done|inapp|<n>", or
                // "card:down|up" to scroll its content like the mouse wheel.
                if let Some(what) = cmd.strip_prefix("card:").filter(|_| cfg!(debug_assertions) && self.visual == Visual::Permission) {
                    return match what {
                        "allow" => self.answer(Decision::Allow),
                        "deny" => self.answer(Decision::Deny),
                        "done" => self.done_choosing(),
                        "inapp" => self.finish(None),
                        "down" | "up" => self.scroll_card(what == "down"),
                        n => n.parse::<usize>().map_or(Ok(()), |i| self.choose(i.saturating_sub(1))),
                    };
                }
                if let Some(n) = cmd.strip_prefix("deck-run:").filter(|_| cfg!(debug_assertions)).and_then(|n| n.parse::<usize>().ok()) {
                    return self.run_deck(n.saturating_sub(1));
                }
                // Debug build only: view another day in the Today tab without a click, "today-day:-2" = two days
                // ago, "today-day:0" = today.
                if let Some(n) = cmd.strip_prefix("today-day:").filter(|_| cfg!(debug_assertions)).and_then(|n| n.parse::<i32>().ok()) {
                    let today = clock::today();
                    let d = if n < 0 { today.back(n.unsigned_abs()) } else { today.ahead(n as u32) };
                    self.ui.tab = Tab::Today;
                    self.ui.editor = None;
                    self.close_input();
                    self.ui.day = (d != today).then_some(d);
                    self.ui.scroll = 0;
                    return if self.env.expanded { self.refresh() } else { self.set_expanded(true) };
                }
                // Debug build only: scroll the Settings page to a position (logical pixels).
                if let Some(px) = cmd.strip_prefix("settings-scroll:").filter(|_| cfg!(debug_assertions)).and_then(|v| v.parse::<f32>().ok()) {
                    self.ui.settings_scroll = px.clamp(0.0, layout::settings_max_scroll());
                    return self.refresh();
                }
                // Debug build only: next prayer reminder right now ("prayer-test:early|now"),
                // or dismiss the showing reminder like the Done button ("prayer-test:done").
                if let Some(when) = cmd.strip_prefix("prayer-test:").filter(|_| cfg!(debug_assertions)) {
                    if when == "done" {
                        return self.dismiss_prayer();
                    }
                    let Some(t) = self.prayer_today() else { return Ok(()) };
                    let now = clock::now();
                    let (i, _) = prayer::next(&t, (now.hour as u32 * 60 + now.minute as u32) as f64);
                    return self.prayer_alert(i, t.prayers[i], when == "early");
                }
                // Debug builds only: simulated end of a pomodoro phase ("focus-test:focus|break|timer").
                if let Some(what) = cmd.strip_prefix("focus-test:").filter(|_| cfg!(debug_assertions)) {
                    let f = match what {
                        "break" => Finished::Break,
                        "timer" => Finished::Timer,
                        _ => Finished::Focus { long: false },
                    };
                    return self.on_focus_finished(f);
                }
                // Debug build only: music state is flipped (playing/paused), to test a media session that
                // flips constantly like a YouTube tab.
                if cmd == "media-flip" && cfg!(debug_assertions) {
                    if let Some(m) = self.media.as_mut() {
                        m.playing = !m.playing;
                    }
                    return self.refresh();
                }
                // Debug build only: Nochi blinks now, to record the blink.
                if cmd == "blink" && cfg!(debug_assertions) {
                    return self.island.pet_blink();
                }
                // Debug build only: stop the adhan like the "Stop adhan" button, then log the state.
                if cmd == "adhan-stop" && cfg!(debug_assertions) {
                    log!("adhan playing before stop: {}", adhan::playing());
                    self.on_widget(WidgetCmd::StopAdhan)?;
                    log!("adhan playing after stop: {}", adhan::playing());
                    return Ok(());
                }
                // Debug build only: fake hover on the Settings page, to see tooltips without a mouse.
                if let Some(name) = cmd.strip_prefix("hover-setting:").filter(|_| cfg!(debug_assertions)) {
                    // Also tabs ("tab-deck") and clipboard rows ("clip0").
                    let other = match name {
                        "tab-today" => Some(Hit::Tab(Tab::Today)),
                        "tab-agent" => Some(Hit::Tab(Tab::Agent)),
                        "tab-focus" => Some(Hit::Tab(Tab::Focus)),
                        "tab-deck" => Some(Hit::Tab(Tab::Deck)),
                        "tab-widgets" => Some(Hit::Tab(Tab::Widgets)),
                        n => n.strip_prefix("clip").and_then(|i| i.parse().ok()).map(|i| Hit::Widget(WidgetCmd::Clip(i))),
                    };
                    if let Some(h) = other {
                        self.ui.hover = Some(h);
                        return self.refresh();
                    }
                    let s = match name {
                        "shy" => Setting::Shy,
                        "fullscreen" => Setting::HideFullscreen,
                        "chips" => Setting::Chips,
                        "autostart" => Setting::Autostart,
                        "sound" => Setting::Sound,
                        "stay" => Setting::FollowApp(false),
                        "follow" => Setting::FollowApp(true),
                        "prayer" => Setting::PrayerWidget,
                        "clip" => Setting::ClipHistory,
                        "remind" => Setting::PrayerRemind,
                        "chime" => Setting::Adhan(false),
                        "adhan" => Setting::Adhan(true),
                        "chat" => Setting::Mcp(McpApp::ClaudeApp),
                        "codex" => Setting::Mcp(McpApp::Codex),
                        "antigravity" => Setting::Mcp(McpApp::Antigravity),
                        "vscode" => Setting::Mcp(McpApp::VsCode),
                        "codex-usage" => Setting::CodexUsage,
                        "copy" => Setting::CopyMcp,
                        "usage" => Setting::ClaudeUsage,
                        _ => Setting::Claude,
                    };
                    self.ui.hover = Some(Hit::Setting(s));
                    return self.refresh();
                }
                // Debug build only: fake click on an empty Agent tab button ("claude", "usage", or
                // an MCP app name like "codex").
                if let Some(name) = cmd.strip_prefix("click-setting:").filter(|_| cfg!(debug_assertions)) {
                    let s = match name {
                        "usage" => Setting::ClaudeUsage,
                        "copy" => Setting::CopyMcp,
                        "codex-usage" => Setting::CodexUsage,
                        n => McpApp::from_key(n).map_or(Setting::Claude, Setting::Mcp),
                    };
                    return self.change_setting(s);
                }
                // Debug builds only: fill the clipboard history without touching the real clipboard.
                if let Some(text) = cmd.strip_prefix("clip-test:").filter(|_| cfg!(debug_assertions)) {
                    self.remember_clip(text.to_string());
                    return self.refresh();
                }
                if let Some(title) = cmd.strip_prefix("media-test:").filter(|_| cfg!(debug_assertions)) {
                    // Debug build only: fake song to test the media buttons and marquee.
                    self.media = Some(NowPlaying { title: title.into(), artist: "Test Artist".into(), playing: true, can_prev: true, can_next: true });
                    self.refresh()
                } else if let Some(text) = cmd.strip_prefix("command:") {
                    self.open_command()?;
                    self.input.set_text(text);
                    Ok(())
                } else if let Some(text) = cmd.strip_prefix("glance:") {
                    self.show_glance(text, Mood::Happy { hop: true })
                } else if let Some(text) = cmd.strip_prefix("add:") {
                    let (n, _) = self.add_from_text(text);
                    if n > 0 && !self.env.expanded {
                        self.show_glance(&format!("{} added", tasks(n)), Mood::Happy { hop: true })
                    } else {
                        self.refresh()
                    }
                } else {
                    Ok(())
                }
            }
        }
    }

    fn schedule_clock(&self) {
        set_timer(self.controller, T_CLOCK, clock::ms_to_next_minute(&clock::now()));
    }

    fn on_timer(&mut self, id: usize) -> Result<()> {
        kill_timer(self.controller, id);
        match id {
            T_SETTLE => {
                let target = self.placement().window_for(&self.shape);
                self.island.set_window(target)?;
                if self.ui.focus_pending {
                    self.ui.focus_pending = false;
                    self.sync_input(true);
                }
                Ok(())
            }
            T_GLANCE => {
                self.env.glance = None;
                self.apply(true)
            }
            T_PERMISSION => self.expire_permissions(),
            T_FOCUS => self.on_focus_timer(),
            T_PET => {
                self.pet_timer = false;
                if let Some(ms) = self.island.pet_tick()? {
                    set_timer(self.controller, T_PET, ms);
                    self.pet_timer = true;
                }
                Ok(())
            }
            T_COUNTDOWN if self.visual == Visual::Permission => self.refresh(),
            T_CHIP if self.env.hovering && self.visual == Visual::Chip => {
                set_timer(self.controller, T_CHIP, CHIP_HOVER_MS);
                Ok(())
            }
            T_CHIP => self.dismiss_chip(),
            T_PRAYER => self.dismiss_prayer(),
            T_SEND => self.on_send_timer(),
            T_GLIDE => self.on_glide(),
            T_ACTIVITY => {
                if self.sync_live() {
                    self.refresh()?;
                }
                Ok(())
            }
            T_ADHAN | T_MUSIC => self.refresh(),
            T_CLOCK => {
                self.schedule_clock();
                self.remind()?;
                self.prayer_tick()?;
                if self.agents.prune(Instant::now()) {
                    self.update()?;
                }
                // Also handles day rollover: "Today" is recomputed from the current date.
                self.refresh()
            }
            T_HOVER_DWELL => {
                self.env.hovering = true;
                self.apply(true)
            }
            T_HOVER_LEAVE => {
                self.env.hovering = false;
                self.apply(true)
            }
            _ => Ok(()),
        }
    }

    /// Glance "<task> · in 10 min" once for each task that has a time.
    fn remind(&mut self) -> Result<()> {
        const AHEAD_MIN: i32 = 10;
        let now = clock::now();
        let iso = now.date().iso();
        if self.reminded.0 != iso {
            self.reminded = (iso.clone(), HashSet::new());
        }
        let minute = now.hour as i32 * 60 + now.minute as i32;
        let due = self.store.today(now.date()).into_iter().find(|t| {
            let at = t.time.as_deref().and_then(|hm| hm.split_once(':')).and_then(|(h, m)| Some(h.parse::<i32>().ok()? * 60 + m.parse::<i32>().ok()?));
            !t.done && t.due == iso && at == Some(minute + AHEAD_MIN) && !self.reminded.1.contains(&t.id)
        });
        match due {
            Some(t) => {
                let (id, text) = (t.id, format!("{} \u{00B7} in {AHEAD_MIN} min", t.title));
                self.reminded.1.insert(id);
                self.show_glance(&text, Mood::Curious)
            }
            None => Ok(()),
        }
    }

    fn on_hover(&mut self, entered: bool) -> Result<()> {
        if entered {
            kill_timer(self.controller, T_HOVER_LEAVE);
            if self.visual == Visual::Peek {
                // Short delay so a cursor just passing over the title bar does not bring up the pill.
                set_timer(self.controller, T_HOVER_DWELL, HOVER_DWELL_MS);
            } else if !self.env.hovering {
                self.env.hovering = true;
                return self.apply(true);
            }
        } else {
            kill_timer(self.controller, T_HOVER_DWELL);
            if self.env.hovering {
                set_timer(self.controller, T_HOVER_LEAVE, HOVER_LEAVE_MS);
            }
            if self.ui.hover.take().is_some() {
                return self.refresh();
            }
        }
        Ok(())
    }

    fn on_click(&mut self, lp: isize) -> Result<()> {
        // While the adhan plays, clicking the closed notch stops it (instead of opening the panel).
        if matches!(self.visual, Visual::Pill | Visual::Glance(_) | Visual::Live | Visual::Peek | Visual::Focus) && adhan::playing() {
            adhan::close();
            return Ok(());
        }
        let (cx, cy) = self.island.to_canvas(lp);
        log!("click {:?} at {cx:.0},{cy:.0} hit={:?} window={:?}", self.visual, self.painted.hit_at(cx, cy), self.island.window());
        if matches!(self.visual, Visual::Permission | Visual::Chip) && self.armed_at.elapsed() < CLICK_GUARD {
            log!("click ignored: card appeared {} ms ago", self.armed_at.elapsed().as_millis());
            return Ok(());
        }
        if self.visual == Visual::Permission {
            let (x, y) = self.island.to_canvas(lp);
            return match self.painted.hit_at(x, y) {
                Some(Hit::Allow) => self.answer(Decision::Allow),
                Some(Hit::Deny) => self.answer(Decision::Deny),
                Some(Hit::Choice(i)) => self.choose(i),
                Some(Hit::Done) => self.done_choosing(),
                // Answered in the agent app: the card is closed without a decision.
                Some(Hit::InApp) => self.finish(None),
                _ => Ok(()),
            };
        }
        if self.visual == Visual::Chip {
            let (x, y) = self.island.to_canvas(lp);
            return match self.painted.hit_at(x, y) {
                Some(Hit::Chip(i)) => self.run_chip(i),
                // The prayer reminder is dismissed only via Done; a click elsewhere only mutes the adhan.
                _ if self.chip.is_none() && self.prayer_now.is_some() => {
                    if adhan::playing() {
                        adhan::close();
                        return self.refresh();
                    }
                    Ok(())
                }
                _ => self.dismiss_chip(),
            };
        }
        if self.visual == Visual::Pill {
            let (x, y) = self.island.to_canvas(lp);
            if let Some(Hit::DeckFlag(i)) = self.painted.hit_at(x, y) {
                return self.run_deck(i);
            }
        }
        if self.hover_controls() {
            let (x, y) = self.island.to_canvas(lp);
            if let Some(Hit::Media(cmd)) = self.painted.hit_at(x, y) {
                // The result comes back via a Windows event (`WM_APP_MEDIA`).
                media::send(cmd);
                return Ok(());
            }
        }
        if !self.env.expanded {
            // Clicking the Live pill opens the Agent tab directly, clicking the countdown opens the Focus tab;
            // otherwise the last tab is used.
            match self.visual {
                Visual::Live => self.ui.tab = Tab::Agent,
                Visual::Focus => self.ui.tab = Tab::Focus,
                _ => {}
            }
            return self.set_expanded(true);
        }
        let (x, y) = self.island.to_canvas(lp);
        let today = clock::today();
        match self.painted.hit_at(x, y) {
            Some(Hit::Tab(Tab::Command)) => self.open_command(),
            Some(Hit::Tab(tab)) => {
                if tab != Tab::Today {
                    self.close_input();
                } else if self.ui.tab == Tab::Today {
                    // Clicking Today again returns to today.
                    self.ui.day = None;
                }
                self.ui.editor = None;
                // The Settings button works like a toggle: click again to return to Today.
                self.ui.tab = if tab == Tab::Settings && self.ui.tab == Tab::Settings { Tab::Today } else { tab };
                self.ui.hover = None;
                self.refresh()
            }
            Some(Hit::Day(i)) => {
                // Clicking today, or the selected day, returns to today.
                let viewed = self.viewed_day(today);
                let d = viewed.unwrap_or(today).monday().ahead(i as u32);
                self.select_day((d != today && Some(d) != viewed).then_some(d))
            }
            Some(Hit::WeekPrev) => self.step_week(false),
            Some(Hit::WeekNext) => self.step_week(true),
            Some(Hit::BackToToday) => {
                self.ui.hover = None;
                self.select_day(None)
            }
            Some(Hit::Toggle(id)) => {
                self.store.toggle(id, today);
                self.save();
                self.refresh()
            }
            Some(Hit::Delete(id)) => {
                self.store.remove(id);
                self.save();
                self.ui.hover = None;
                self.refresh()
            }
            Some(Hit::Setting(s)) => self.change_setting(s),
            Some(Hit::Media(cmd)) => {
                // The result comes back via a Windows event (`WM_APP_MEDIA`), not guessed here.
                media::send(cmd);
                Ok(())
            }
            Some(Hit::Agent(i)) => {
                // Click opens the row to see the last prompt and action; click again closes it.
                let id = self.agents.sessions_by_state().get(i).map(|s| s.id.clone());
                self.ui.agent_open = if self.ui.agent_open == id { None } else { id };
                self.refresh()
            }
            Some(Hit::Widget(cmd)) => self.on_widget(cmd),
            Some(Hit::Focus(FocusCmd::Toggle)) => self.focus_toggle(),
            Some(Hit::Focus(FocusCmd::Stop)) => self.focus_stop(),
            Some(Hit::Focus(FocusCmd::Task)) => self.focus_next_task(),
            Some(Hit::Cmd(i)) => self.run_cmd(i),
            Some(Hit::CmdProject) => {
                self.ui.cmd_project += 1;
                self.refresh()
            }
            Some(Hit::DeckKey(i)) => self.run_deck(i),
            Some(Hit::DeckAdd) => self.open_editor(None),
            Some(Hit::Edit(cmd)) => self.on_edit(cmd),
            Some(Hit::PinToggle(i)) => self.toggle_pin(i),
            Some(Hit::Add) => self.open_input(),
            Some(Hit::Send) if self.ui.input_open && !self.input.text().trim().is_empty() => self.submit_input(),
            Some(Hit::Send) => self.open_input(),
            _ => Ok(()),
        }
    }

    /// Song, playback state, or media session changed.
    fn on_media(&mut self) -> Result<()> {
        let next = media::latest();
        if next == self.media {
            return Ok(());
        }
        log!("media: {next:?}");
        let was_playing = self.media.as_ref().is_some_and(|m| m.playing);
        match &next {
            Some(m) if m.playing => self.media_paused_at = None,
            Some(_) if was_playing => self.media_paused_at = Some(Instant::now()),
            None => self.media_paused_at = None,
            Some(_) => {}
        }
        self.media = next;
        self.refresh()
    }

    fn save_settings(&self) {
        if let Err(e) = self.settings.save() {
            log!("failed to save settings: {e}");
        }
    }

    /// Click on the Settings page. Notch behavior changes (shy, fullscreen) apply once the panel is closed.
    fn change_setting(&mut self, s: Setting) -> Result<()> {
        match s {
            Setting::Shy => self.settings.shy = !self.settings.shy,
            Setting::HideFullscreen => self.settings.hide_fullscreen = !self.settings.hide_fullscreen,
            Setting::FollowApp(follow) => {
                let mode = if follow { MonitorMode::Follow } else { MonitorMode::Stay };
                if mode != self.settings.monitor {
                    self.settings.monitor = mode;
                    // Fixed: on the monitor where the notch currently is.
                    self.settings.display = display::device_name(self.monitor);
                    self.save_settings();
                    self.monitor = initial_monitor(&self.settings);
                    return self.relayout();
                }
            }
            Setting::Autostart => autostart::set(!autostart::is_enabled())?,
            Setting::Chips => self.settings.chips = !self.settings.chips,
            Setting::Size(size) => self.settings.size = size,
            Setting::PrayerWidget => self.settings.prayer_widget = !self.settings.prayer_widget,
            Setting::PrayerRemind => self.settings.prayer_remind = !self.settings.prayer_remind,
            Setting::Adhan(on) => {
                self.settings.adhan = on;
                if !on {
                    adhan::close();
                }
            }
            Setting::Sound => {
                self.settings.sound = !self.settings.sound;
                if !self.settings.sound {
                    adhan::close();
                }
                // Sample chime when turned on.
                self.play(Chime::Done);
            }
            Setting::ClipHistory => {
                self.settings.clip_history = !self.settings.clip_history;
                self.clips.clear();
            }
            // Applies from the next phase; the running phase is unchanged.
            Setting::Pomodoro(l, up) => self.settings.pomodoro.step(l, up),
            Setting::Claude => {
                if let Some(on) = agent_setup::claude_connected() {
                    if !agent_setup::set_claude(!on) {
                        log!("notch-bridge failed to change the Claude Code hooks");
                    }
                }
                self.hooks.set(None);
            }
            Setting::ClaudeUsage => {
                self.settings.claude_usage = !self.settings.claude_usage;
                // Turned on: request it right away instead of waiting for the next interval.
                self.quota_at = None;
                self.maybe_quota(Duration::ZERO);
            }
            Setting::CodexUsage => {
                self.settings.codex_usage = !self.settings.codex_usage;
                self.codex_at = None;
                self.maybe_quota(Duration::ZERO);
            }
            Setting::Mcp(app) => {
                if let Some(on) = agent_setup::mcp_connected(app) {
                    if !agent_setup::set_mcp(app, !on) {
                        log!("notch-bridge failed to change the {} MCP configuration", app.name());
                    }
                }
                self.hooks.set(None);
            }
            Setting::CopyMcp => {
                let exe = std::env::current_exe().map(|p| p.with_file_name("notch-bridge.exe").display().to_string()).unwrap_or_default();
                let entry = serde_json::json!({ "mcpServers": { "notch": { "command": exe, "args": ["mcp"] } } });
                let text = serde_json::to_string_pretty(&entry).unwrap_or_default();
                self.own_clip_at = Some(Instant::now());
                if launcher::copy_text(self.controller, &text) {
                    self.mcp_copied = Some(Instant::now());
                }
            }
        }
        self.save_settings();
        self.evaluate();
        self.refresh()
    }

    fn on_mouse_move(&mut self, lp: isize) -> Result<()> {
        if !matches!(self.visual, Visual::Expanded | Visual::Permission | Visual::Chip) && !self.hover_controls() {
            return Ok(());
        }
        let (x, y) = self.island.to_canvas(lp);
        let hover = self.painted.hit_at(x, y).filter(|h| *h != Hit::List);
        if hover != self.ui.hover {
            self.ui.hover = hover;
            self.refresh()?;
        }
        Ok(())
    }

    fn on_wheel(&mut self, wp: usize) -> Result<()> {
        if self.visual == Visual::Permission {
            return self.scroll_card(((wp >> 16) as u16 as i16) < 0);
        }
        if self.visual != Visual::Expanded || self.ui.tab == Tab::Command {
            return Ok(());
        }
        if self.ui.tab == Tab::Deck {
            if self.ui.editor.is_none() {
                let delta = (wp >> 16) as u16 as i16;
                let page = self.ui.deck_page.min(self.deck_pages() - 1);
                let next = if delta > 0 { page.saturating_sub(1) } else { (page + 1).min(self.deck_pages() - 1) };
                if next != page {
                    self.ui.deck_page = next;
                    self.ui.hover = None;
                    return self.refresh();
                }
            }
            return Ok(());
        }
        if self.ui.tab == Tab::Agent {
            let delta = (wp >> 16) as u16 as i16;
            let step = if delta > 0 { -layout::AGENT_PITCH } else { layout::AGENT_PITCH };
            let next = (self.ui.agent_scroll + step).clamp(0.0, self.agent_max_scroll());
            if next != self.ui.agent_scroll {
                self.ui.agent_scroll = next;
                self.ui.hover = None;
                self.refresh()?;
            }
            return Ok(());
        }
        if self.ui.tab == Tab::Settings {
            let delta = (wp >> 16) as u16 as i16;
            let step = if delta > 0 { -layout::SETTINGS_STEP } else { layout::SETTINGS_STEP };
            let next = (self.ui.settings_scroll + step).clamp(0.0, layout::settings_max_scroll());
            if next != self.ui.settings_scroll {
                self.ui.settings_scroll = next;
                self.ui.hover = None;
                self.refresh()?;
            }
            return Ok(());
        }
        let delta = (wp >> 16) as u16 as i16;
        // Wheel over the week strip shifts the week: up goes back, down goes forward.
        if matches!(self.ui.hover, Some(Hit::Day(_) | Hit::WeekPrev | Hit::WeekNext)) {
            return self.step_week(delta < 0);
        }
        let (_, total, ..) = self.visible_tasks(clock::today());
        let max = total.saturating_sub(layout::task_rows_fit(self.preview_lines(), self.media.is_some()));
        let current = self.ui.scroll.min(max);
        self.ui.scroll = if delta > 0 { current.saturating_sub(1) } else { (current + 1).min(max) };
        if self.ui.scroll != current {
            self.ui.hover = None;
            self.refresh()?;
        }
        Ok(())
    }

    fn on_escape(&mut self, outside_click: bool) -> Result<()> {
        if !self.env.expanded {
            return Ok(());
        }
        if !outside_click && self.ui.editor.is_some() {
            // Esc in the deck editor: back to the grid without saving.
            return self.close_editor();
        }
        if outside_click {
            self.outside_collapse_at = Some(Instant::now());
        } else if self.ui.input_open && self.ui.tab != Tab::Command {
            // First Esc closes the input field, second Esc closes the panel.
            self.close_input();
            return self.refresh();
        }
        self.set_expanded(false)
    }

    fn on_win_event(&mut self, event: u32, hwnd: HWND) -> Result<()> {
        if event == EVENT_SYSTEM_FOREGROUND {
            log!("foreground: {} ignored={}", watch::describe(hwnd), watch::is_ignored(hwnd));
        }
        if event == EVENT_SYSTEM_FOREGROUND && !watch::is_ignored(hwnd) {
            self.last_app = hwnd;
            self.watcher.follow(hwnd);
            if self.settings.monitor == MonitorMode::Follow {
                let monitor = display::monitor_of(hwnd);
                if monitor != self.monitor {
                    log!("moved to monitor -> {:?}", display::bounds(monitor));
                    self.monitor = monitor;
                    return self.relayout();
                }
            }
        }
        self.evaluate();
        self.apply(true)?;
        self.island.reassert_topmost();
        self.input.raise();
        Ok(())
    }

    /// Monitor, resolution, or DPI changed: recompute positions and redraw without animation.
    fn relayout(&mut self) -> Result<()> {
        if display::bounds(self.monitor).is_none() {
            self.monitor = display::primary();
        }
        self.island.set_placement(self.placement())?;
        self.evaluate();
        self.apply(false)
    }

    fn on_tray_click(&mut self) -> Result<()> {
        if self.outside_collapse_at.is_some_and(|t| t.elapsed() < REOPEN_GUARD) {
            return Ok(());
        }
        self.toggle()
    }

    fn on_menu(&mut self, cmd: MenuCmd) -> Result<()> {
        match cmd {
            MenuCmd::Toggle => self.toggle(),
            MenuCmd::TestGlance => self.show_glance("Claude finished · notch · 3 files changed", Mood::Happy { hop: true }),
            MenuCmd::MonitorFollow | MenuCmd::MonitorStay => {
                self.settings.monitor = if cmd == MenuCmd::MonitorFollow { MonitorMode::Follow } else { MonitorMode::Stay };
                self.settings.display = display::device_name(self.monitor);
                self.save_settings();
                self.monitor = initial_monitor(&self.settings);
                self.relayout()
            }
            MenuCmd::Autostart => autostart::set(!autostart::is_enabled()),
            MenuCmd::Quit => {
                unsafe { PostQuitMessage(0) };
                Ok(())
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            let _ = UnregisterHotKey(Some(self.controller), HOTKEY_TOGGLE);
            let _ = UnregisterHotKey(Some(self.controller), HOTKEY_ADD);
            let _ = UnregisterHotKey(Some(self.controller), HOTKEY_FOCUS);
            for i in 0..10 {
                let _ = UnregisterHotKey(Some(self.controller), HOTKEY_DECK + i);
            }
        }
        dragdrop::revoke(self.island.hwnd);
        chips::stop(self.controller);
    }
}

/// Open button for something dragged onto the notch.
fn dropped_button(d: &Dropped) -> Button {
    match d {
        Dropped::Path(p) => {
            let glyph = if std::path::Path::new(p).is_dir() { Glyph::Folder } else { Glyph::App };
            Button { glyph, value: p.clone(), ..Button::new(deck::Kind::Open) }
        }
        Dropped::Link(url) => {
            let host = command::link(url).map_or_else(|| url.clone(), |(host, _)| host);
            Button { label: host, glyph: Glyph::Globe, value: url.clone(), ..Button::new(deck::Kind::Open) }
        }
    }
}

/// Deck key target for a search result: projects open in VS Code via a `vscode://` link.
fn deck_target(e: &Entry) -> String {
    match e.kind {
        Kind::Project => format!("vscode://file/{}", e.target.replace('\\', "/")),
        Kind::App | Kind::Folder => e.target.clone(),
    }
}

/// Opens an app, folder, or project (in VS Code).
fn open_entry(e: &Entry) {
    match e.kind {
        Kind::Project => launcher::open_project(&e.target),
        Kind::App | Kind::Folder => launcher::open(&e.target),
    };
}

const FALLBACK_PLACEMENT: Placement = Placement { monitor: Rect { x: 0, y: 0, w: 1920, h: 1080 }, scale: 1.0 };

/// Notch monitor: the chosen one (fixed), or the active window's monitor (follows). A chosen monitor that
/// is not connected is replaced by the primary monitor until it reconnects.
fn initial_monitor(settings: &Settings) -> HMONITOR {
    match settings.monitor {
        MonitorMode::Follow => {
            let fg = watch::foreground();
            if watch::is_ignored(fg) {
                display::primary()
            } else {
                display::monitor_of(fg)
            }
        }
        MonitorMode::Stay => display::by_device(&settings.display).unwrap_or_else(display::primary),
    }
}

fn tray_menu(owner: HWND) {
    let state = APP.with(|cell| cell.try_borrow().ok().and_then(|g| g.as_ref().map(|a| (a.settings.monitor, a.toggle_key_ok))));
    let Some((monitor, hotkey_ok)) = state else { return };
    // Menu opens without holding state: its modal loop still processes other events.
    if let Some(cmd) = tray::show_menu(owner, monitor, autostart::is_enabled(), hotkey_ok) {
        with_app(|a| a.on_menu(cmd));
    }
}

extern "system" fn controller_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => match wp.0 as i32 {
            HOTKEY_ADD => with_app(|a| a.open_input()),
            HOTKEY_FOCUS => with_app(|a| a.focus_toggle()),
            id @ HOTKEY_DECK..=19 => with_app(|a| a.run_deck((id - HOTKEY_DECK) as usize)),
            _ => with_app(|a| a.toggle_command()),
        },
        WM_TIMER => with_app(|a| a.on_timer(wp.0)),
        WM_APP_TRAY => match lp.0 as u32 {
            WM_LBUTTONUP => with_app(|a| a.on_tray_click()),
            WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(hwnd),
            _ => {}
        },
        WM_APP_WINEVENT => with_app(|a| a.on_win_event(wp.0 as u32, HWND(lp.0 as *mut _))),
        WM_APP_CLICK => with_app(|a| a.on_click(lp.0)),
        WM_APP_MOUSEMOVE => with_app(|a| a.on_mouse_move(lp.0)),
        WM_APP_WHEEL => with_app(|a| a.on_wheel(wp.0)),
        WM_APP_HOVER_ENTER => with_app(|a| a.on_hover(true)),
        WM_APP_HOVER_LEAVE => with_app(|a| a.on_hover(false)),
        WM_APP_OUTSIDE_CLICK => with_app(|a| a.on_escape(true)),
        WM_APP_ESCAPE => with_app(|a| a.on_escape(false)),
        WM_APP_INPUT_CHANGED => with_app(|a| {
            let text = a.input.text();
            if !text.is_empty() {
                a.ui.notice = None;
            }
            if a.ui.editor.is_some() {
                // Editor field: its text is only applied when switching fields or saving.
                return Ok(());
            }
            if a.ui.tab == Tab::Command {
                a.ui.cmd_selected = 0;
            } else {
                a.ui.preview = parse(&text);
            }
            a.refresh()
        }),
        WM_APP_INPUT_KEY => with_app(|a| a.on_input_key(wp.0 as u16)),
        WM_APP_RCLICK => with_app(|a| a.on_right_click(lp.0)),
        WM_APP_DECK_KEY => with_app(|a| a.on_recorded(wp.0 as u16, lp.0 as usize)),
        WM_APP_DRAG => {
            // For ENTER and DROP, the pointer is created by the OLE callback with Box::into_raw.
            let items = (wp.0 != dragdrop::LEAVE).then(|| unsafe { *Box::from_raw(lp.0 as *mut Vec<Dropped>) });
            with_app(|a| match (wp.0, items) {
                (dragdrop::ENTER, Some(items)) => a.on_drag_enter(&items),
                (dragdrop::DROP, Some(items)) => a.on_drop(items),
                _ => a.on_drag_leave(),
            });
        }
        WM_APP_DOWNLOAD => {
            // Pointer created by the Downloads watcher thread; ownership moves here.
            let path = unsafe { *Box::from_raw(lp.0 as *mut String) };
            with_app(|a| a.on_download(path));
        }
        chips::WM_CLIPBOARDUPDATE => with_app(|a| a.on_clipboard()),
        chips::WM_WTSSESSION_CHANGE => with_app(|a| match wp.0 {
            chips::WTS_SESSION_LOCK => {
                a.locked_at = Some(Instant::now());
                Ok(())
            }
            chips::WTS_SESSION_UNLOCK => a.maybe_greet(true),
            _ => Ok(()),
        }),
        WM_APP_INPUT_SUBMIT => with_app(|a| {
            if a.ui.editor.is_some() {
                a.on_edit(EditCmd::Save)
            } else if a.ui.tab == Tab::Command {
                let selected = a.command_items().0.selected;
                a.run_cmd(selected)
            } else {
                a.submit_input()
            }
        }),
        WM_APP_AGENT => {
            // Pointer created by the pipe thread with Box::into_raw; ownership moves here.
            let incoming = unsafe { *Box::from_raw(lp.0 as *mut Incoming) };
            with_app(|a| a.on_agent(incoming));
        }
        WM_APP_MEDIA => with_app(|a| a.on_media()),
        WM_APP_QUOTA => with_app(|a| a.on_quota()),
        WM_APP_LOCATION => with_app(|a| a.on_location()),
        // Adhan finished (or stopped): file closed so nothing stays held in memory.
        windows::Win32::Media::MM_MCINOTIFY => {
            adhan::close();
            // Prayer card in Widgets and the prayer reminder caption ("Adhan playing") are updated.
            with_app(|a| {
                if (a.env.expanded && a.ui.tab == Tab::Widgets) || (a.visual == Visual::Chip && a.chip.is_none() && a.prayer_now.is_some()) {
                    a.refresh()
                } else {
                    Ok(())
                }
            })
        }
        WM_APP_RELAYOUT => with_app(|a| {
            // Monitor unplugged or reconnected: look up the chosen monitor again.
            if wp.0 == 1 && a.settings.monitor == MonitorMode::Stay {
                a.monitor = initial_monitor(&a.settings);
            }
            a.relayout()
        }),
        WM_APP_MOVE => with_app(|a| a.on_move(wp.0, lp.0)),
        // Can be sent synchronously in the middle of another operation, so it is processed later.
        WM_DISPLAYCHANGE => watch::post(WM_APP_RELAYOUT, 1, 0),
        WM_SETTINGCHANGE => watch::post(WM_APP_RELAYOUT, 0, 0),
        WM_COPYDATA => {
            let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
            let data = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
            let cmd = String::from_utf16_lossy(data);
            APP.with(|cell| {
                if cell.try_borrow_mut().is_ok() {
                    command(&cmd);
                }
            });
            return LRESULT(1);
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => with_app(|a| {
            a.tray.readd();
            Ok(())
        }),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

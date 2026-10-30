//! Local tasks, stored in %APPDATA%\Notch\tasks.json. No account needed;
//! connectors like Microsoft To Do will later sync from here.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clock::Date;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Task {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub done: bool,
    /// Due date, `YYYY-MM-DD`.
    pub due: String,
    /// Optional time, `HH:MM`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// Unix seconds when created.
    pub created: u64,
    /// Completion date, `YYYY-MM-DD`. Basis of the week strip's history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<String>,
}

/// Completed tasks are kept this long so earlier days can still be viewed; older ones
/// are dropped, so the file does not grow forever.
pub const HISTORY_DAYS: u32 = 60;

impl Task {
    /// The day this task was completed. A hand-edited file may have `done` without `completed`;
    /// its due date is used then.
    fn done_on(&self) -> Option<&str> {
        self.done.then(|| self.completed.as_deref().unwrap_or(&self.due))
    }
}

/// List order: those with a time first by time, then creation order.
fn by_time(a: &&Task, b: &&Task) -> std::cmp::Ordering {
    (a.time.is_none(), &a.time, a.id).cmp(&(b.time.is_none(), &b.time, b.id))
}

/// Target day when the week strip is moved by a week from the day being viewed (`viewed`).
/// Going back stops at the history limit; going forward that reaches this week returns to today.
/// `None` if it cannot move.
pub fn week_step(viewed: Date, today: Date, forward: bool) -> Option<Date> {
    let this_week = today.monday();
    if forward {
        (viewed.monday() < this_week).then(|| {
            let next = viewed.ahead(7);
            if next >= this_week {
                today
            } else {
                next
            }
        })
    } else {
        let oldest = today.back(HISTORY_DAYS);
        (viewed.monday() > oldest).then(|| viewed.back(7).max(oldest))
    }
}

#[derive(Serialize, Deserialize)]
struct TaskFile {
    version: u32,
    next_id: u64,
    tasks: Vec<Task>,
}

impl Default for TaskFile {
    fn default() -> Self {
        Self { version: 1, next_id: 1, tasks: Vec::new() }
    }
}

pub struct Store {
    path: Option<PathBuf>,
    file: TaskFile,
}

fn default_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Notch").join("tasks.json"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Store {
    pub fn open_default() -> Self {
        match default_path() {
            Some(p) => Self::open(&p),
            None => Self { path: None, file: TaskFile::default() },
        }
    }

    /// A corrupt file is not discarded: it is renamed to `tasks.corrupt-<time>.json`, then start empty.
    pub fn open(path: &Path) -> Self {
        let file = match std::fs::read_to_string(path) {
            // A BOM is tolerated: Notepad and PowerShell 5 add one when saving UTF-8.
            Ok(text) => serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_else(|_| {
                let _ = std::fs::rename(path, path.with_file_name(format!("tasks.corrupt-{}.json", unix_now())));
                TaskFile::default()
            }),
            Err(_) => TaskFile::default(),
        };
        Self { path: Some(path.to_path_buf()), file }
    }

    /// Written to a temp file then renamed, so the file is never half-written.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.file).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }

    pub fn add(&mut self, title: &str, due: Date, time: Option<(u8, u8)>) -> u64 {
        let id = self.file.next_id;
        self.file.next_id += 1;
        self.file.tasks.push(Task {
            id,
            title: title.to_string(),
            done: false,
            due: due.iso(),
            time: time.map(|(h, m)| format!("{h:02}:{m:02}")),
            created: unix_now(),
            completed: None,
        });
        id
    }

    pub fn toggle(&mut self, id: u64, today: Date) -> bool {
        let Some(t) = self.file.tasks.iter_mut().find(|t| t.id == id) else { return false };
        t.done = !t.done;
        t.completed = t.done.then(|| today.iso());
        true
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.file.tasks.len();
        self.file.tasks.retain(|t| t.id != id);
        self.file.tasks.len() != before
    }

    /// Due dates (`YYYY-MM-DD`) of unfinished tasks, for the markers in the week strip.
    pub fn open_due_dates(&self) -> std::collections::HashSet<&str> {
        self.file.tasks.iter().filter(|t| !t.done).map(|t| t.due.as_str()).collect()
    }

    /// "Today" view: unfinished tasks due today or earlier,
    /// then those completed today. Order: overdue first, then by time, then creation order.
    pub fn today(&self, today: Date) -> Vec<&Task> {
        let today = today.iso();
        let mut open: Vec<&Task> = self.file.tasks.iter().filter(|t| !t.done && t.due <= today).collect();
        open.sort_by(|a, b| {
            (a.due == today, a.time.is_none(), &a.time, a.id).cmp(&(b.due == today, b.time.is_none(), &b.time, b.id))
        });
        let done = self.file.tasks.iter().filter(|t| t.done && t.completed.as_deref() == Some(today.as_str()));
        open.extend(done);
        open
    }

    /// Dates (`YYYY-MM-DD`) that have completed tasks, for the past-day markers in the week strip.
    pub fn done_dates(&self) -> std::collections::HashSet<&str> {
        self.file.tasks.iter().filter_map(Task::done_on).collect()
    }

    /// View of a single day other than today: unfinished tasks due exactly that day, then
    /// those completed that day. Overdue tasks from other days are left out, so each day only
    /// shows its own.
    pub fn on_day(&self, day: Date) -> Vec<&Task> {
        let day = day.iso();
        let mut open: Vec<&Task> = self.file.tasks.iter().filter(|t| !t.done && t.due == day).collect();
        open.sort_by(by_time);
        let mut done: Vec<&Task> = self.file.tasks.iter().filter(|t| t.done_on() == Some(day.as_str())).collect();
        done.sort_by(by_time);
        open.extend(done);
        open
    }

    /// Drops completed tasks older than `HISTORY_DAYS`. Unfinished ones are never
    /// dropped. Returns the number dropped; save afterwards if more than zero.
    pub fn prune(&mut self, today: Date) -> usize {
        let oldest = today.back(HISTORY_DAYS).iso();
        let before = self.file.tasks.len();
        self.file.tasks.retain(|t| t.done_on().is_none_or(|d| d >= oldest.as_str()));
        before - self.file.tasks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: Date = Date { y: 2026, m: 10, d: 22 };
    const YESTERDAY: Date = Date { y: 2026, m: 10, d: 21 };

    /// Temporary folder dedicated to each test, removed at the end of the test.
    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("notch-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("tasks.json")
    }

    #[test]
    fn today_view_orders_and_filters() {
        let mut s = Store { path: None, file: TaskFile::default() };
        let plain = s.add("Balas email", TODAY, None);
        let meeting = s.add("Meeting", TODAY, Some((14, 0)));
        let late = s.add("Laporan", YESTERDAY, None);
        let _tomorrow = s.add("Besok", TODAY.next(), None);
        let standup = s.add("Standup", TODAY, Some((9, 0)));
        s.toggle(standup, TODAY);
        let ids: Vec<u64> = s.today(TODAY).iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![late, meeting, plain, standup]);
    }

    #[test]
    fn toggle_and_remove() {
        let mut s = Store { path: None, file: TaskFile::default() };
        let id = s.add("A", TODAY, None);
        assert!(s.toggle(id, TODAY));
        assert_eq!(s.today(TODAY)[0].completed.as_deref(), Some("2026-10-22"));
        assert!(s.toggle(id, TODAY));
        assert!(!s.today(TODAY)[0].done);
        assert!(s.remove(id) && !s.remove(id));
    }

    #[test]
    fn save_and_reopen() {
        let path = temp_path("roundtrip");
        let mut s = Store::open(&path);
        s.add("Review PR", TODAY, Some((9, 30)));
        s.save().unwrap();
        let again = Store::open(&path);
        assert_eq!(again.today(TODAY)[0].title, "Review PR");
        assert_eq!(again.today(TODAY)[0].time.as_deref(), Some("09:30"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn past_day_shows_done_and_open_due_then() {
        let mut s = Store { path: None, file: TaskFile::default() };
        let before = YESTERDAY.prev();
        let late = s.add("Masih terbuka", YESTERDAY, None);
        let older = s.add("Lebih lama", before, None);
        let done_then = s.add("Selesai kemarin", before, Some((10, 0)));
        s.toggle(done_then, YESTERDAY);
        let done_today = s.add("Selesai hari ini", YESTERDAY, None);
        s.toggle(done_today, TODAY);
        let ids = |v: Vec<&Task>| v.iter().map(|t| t.id).collect::<Vec<u64>>();
        // Yesterday: those due yesterday and unfinished, then those completed yesterday.
        assert_eq!(ids(s.on_day(YESTERDAY)), vec![late, done_then]);
        // Overdue tasks do not carry to other days; completed ones move to the day they were completed.
        assert_eq!(ids(s.on_day(before)), vec![older]);
        assert!(s.on_day(before.prev()).is_empty());
        // Today keeps the usual view: all overdue ones carry over.
        assert_eq!(ids(s.today(TODAY)), vec![late, older, done_today]);
        assert!(s.done_dates().contains("2026-10-21") && s.done_dates().contains("2026-10-22"));
    }

    #[test]
    fn prune_keeps_recent_history_and_open_tasks() {
        let mut s = Store { path: None, file: TaskFile::default() };
        let edge = TODAY.back(HISTORY_DAYS);
        let kept = s.add("Tepat di batas", edge, None);
        s.toggle(kept, edge);
        let gone = s.add("Terlalu lama", edge.prev(), None);
        s.toggle(gone, edge.prev());
        let open = s.add("Lama tapi belum selesai", edge.back(30), None);
        assert_eq!(s.prune(TODAY), 1);
        let left: Vec<u64> = s.file.tasks.iter().map(|t| t.id).collect();
        assert_eq!(left, vec![kept, open]);
        assert_eq!(s.prune(TODAY), 0);
    }

    #[test]
    fn old_format_files_still_load() {
        // Old format: no `time`, no `completed`, including completed tasks without a completion date.
        let path = temp_path("oldformat");
        std::fs::write(
            &path,
            r#"{"version":1,"next_id":3,"tasks":[
                {"id":1,"title":"Lama","due":"2026-10-20","created":1},
                {"id":2,"title":"Selesai","done":true,"due":"2026-10-21","created":2}
            ]}"#,
        )
        .unwrap();
        let mut s = Store::open(&path);
        assert!(path.exists());
        assert_eq!(s.today(TODAY)[0].title, "Lama");
        // Without `completed`, the due date counts as the completion day.
        assert_eq!(s.on_day(YESTERDAY)[0].id, 2);
        assert_eq!(s.prune(TODAY), 0);
        assert_eq!(s.prune(Date { y: 2027, m: 1, d: 1 }), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn week_navigation() {
        // Friday, 30 October 2026; this week starts Monday the 26th.
        let today = Date { y: 2026, m: 10, d: 30 };
        let d = |m, d| Date { y: 2026, m, d };
        assert_eq!(week_step(today, today, false), Some(d(10, 23)));
        assert_eq!(week_step(d(10, 23), today, true), Some(today));
        assert_eq!(week_step(d(10, 14), today, true), Some(d(10, 21)));
        // Does not go into the future past this week.
        assert_eq!(week_step(today, today, true), None);
        assert_eq!(week_step(d(10, 27), today, true), None);
        // Going back stops at the week containing the history limit (Monday 31 August).
        assert_eq!(week_step(d(9, 9), today, false), Some(d(9, 2)));
        assert_eq!(week_step(d(9, 4), today, false), None);
        // Limit mid-week: the target does not pass the limit (Saturday 31 October, limit Tuesday 1 September).
        let saturday = d(10, 31);
        assert_eq!(saturday.back(HISTORY_DAYS), d(9, 1));
        assert_eq!(week_step(d(9, 7), saturday, false), Some(d(9, 1)));
        assert_eq!(week_step(d(9, 1), saturday, false), None);
    }

    #[test]
    fn corrupt_file_is_kept_aside() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "{ bukan json").unwrap();
        let s = Store::open(&path);
        assert!(s.today(TODAY).is_empty());
        assert!(!path.exists());
        let dir = path.parent().unwrap();
        let kept = std::fs::read_dir(dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("tasks.corrupt-"));
        assert!(kept);
        let _ = std::fs::remove_dir_all(dir);
    }
}

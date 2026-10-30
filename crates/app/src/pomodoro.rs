//! Pomodoro timer: focus, short break, and long break every few sessions. Pure (no
//! Win32) so it is easy to test. The app does not count down itself: it sets a single one-shot timer
//! for when the displayed text changes (see `next_wake`).

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Focus,
    ShortBreak,
    LongBreak,
    /// Plain countdown from the command bar ("10m tea"): no break afterwards.
    Timer,
}

/// Values that can be set in Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Length {
    Focus,
    ShortBreak,
    LongBreak,
    /// Long break after this many focus sessions.
    Every,
}

impl Length {
    /// (min, max, step of the − / + buttons).
    fn range(self) -> (u32, u32, u32) {
        match self {
            Length::Focus => (5, 90, 5),
            Length::ShortBreak => (1, 30, 1),
            Length::LongBreak => (5, 60, 5),
            Length::Every => (2, 8, 1),
        }
    }
}

/// Length of each phase (minutes) and number of sessions before the long break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Durations {
    pub focus: u32,
    pub short_break: u32,
    pub long_break: u32,
    pub every: u32,
}

impl Default for Durations {
    fn default() -> Self {
        Self { focus: 25, short_break: 5, long_break: 15, every: 4 }
    }
}

impl Durations {
    pub fn get(&self, l: Length) -> u32 {
        match l {
            Length::Focus => self.focus,
            Length::ShortBreak => self.short_break,
            Length::LongBreak => self.long_break,
            Length::Every => self.every,
        }
    }

    /// Out-of-range values (e.g. from a hand-edited settings.ini) are clamped to the limits.
    pub fn set(&mut self, l: Length, v: u32) {
        let (lo, hi, _) = l.range();
        let v = v.clamp(lo, hi);
        match l {
            Length::Focus => self.focus = v,
            Length::ShortBreak => self.short_break = v,
            Length::LongBreak => self.long_break = v,
            Length::Every => self.every = v,
        }
    }

    /// The − / + buttons in Settings.
    pub fn step(&mut self, l: Length, up: bool) {
        let (_, _, step) = l.range();
        let v = self.get(l);
        self.set(l, if up { v + step } else { v.saturating_sub(step) });
    }

    pub fn of(&self, phase: Phase) -> Duration {
        let minutes = match phase {
            Phase::Focus => self.focus,
            Phase::ShortBreak => self.short_break,
            Phase::LongBreak => self.long_break,
            // The length is set when it starts; never waits to be started.
            Phase::Timer => 0,
        };
        Duration::from_secs(minutes as u64 * 60)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Run {
    Idle,
    Running { ends: Instant },
    Paused { left: Duration },
}

/// How often the countdown text needs to be updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// "18:42": clearly visible (hovered, panel open).
    Second,
    /// "19 min": compact notch.
    Minute,
    /// No visible text; only needed when the phase ends.
    End,
}

/// The phase that just ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finished {
    /// The break has started automatically; `long` if it is a long break.
    Focus { long: bool },
    /// The next focus is waiting to be started.
    Break,
    /// Plain countdown finished.
    Timer,
}

#[derive(Debug)]
pub struct Pomodoro {
    phase: Phase,
    run: Run,
    /// Length of the current phase, for the progress ring.
    length: Duration,
    /// Focus sessions completed in this cycle (back to 0 after the long break).
    done: u32,
}

impl Default for Pomodoro {
    fn default() -> Self {
        Self { phase: Phase::Focus, run: Run::Idle, length: Duration::ZERO, done: 0 }
    }
}

impl Pomodoro {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Running or paused.
    pub fn is_active(&self) -> bool {
        self.run != Run::Idle
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.run, Run::Paused { .. })
    }

    pub fn done(&self) -> u32 {
        self.done
    }

    /// Time remaining; before starting, the length of the next phase.
    pub fn remaining(&self, now: Instant, d: &Durations) -> Duration {
        match self.run {
            Run::Idle => d.of(self.phase),
            Run::Running { ends } => ends.saturating_duration_since(now),
            Run::Paused { left } => left,
        }
    }

    /// Fraction of the phase elapsed, 0..1.
    pub fn progress(&self, now: Instant, d: &Durations) -> f32 {
        if self.run == Run::Idle || self.length.is_zero() {
            return 0.0;
        }
        (1.0 - self.remaining(now, d).as_secs_f32() / self.length.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// Start, pause, or resume.
    pub fn toggle(&mut self, now: Instant, d: &Durations) {
        self.run = match self.run {
            Run::Idle => {
                self.length = d.of(self.phase);
                Run::Running { ends: now + self.length }
            }
            Run::Running { ends } => Run::Paused { left: ends.saturating_duration_since(now) },
            Run::Paused { left } => Run::Running { ends: now + left },
        };
    }

    /// Start now with a given length: a focus session from the command bar ("25m review PR"), or
    /// a plain countdown (`focus = false`). Whatever is running is replaced.
    pub fn start(&mut self, now: Instant, length: Duration, focus: bool) {
        self.phase = if focus { Phase::Focus } else { Phase::Timer };
        self.length = length;
        self.run = Run::Running { ends: now + length };
    }

    /// Stops the current phase (focus or break); the next one is focus again. Sessions already
    /// completed still count.
    pub fn stop(&mut self) {
        self.run = Run::Idle;
        self.phase = Phase::Focus;
    }

    /// Called by the app timer. A finished focus goes straight into a break; a finished break
    /// stops and waits for the next focus to be started.
    pub fn tick(&mut self, now: Instant, d: &Durations) -> Option<Finished> {
        let Run::Running { ends } = self.run else { return None };
        if now < ends {
            return None;
        }
        match self.phase {
            Phase::Focus => {
                self.done += 1;
                let long = self.done.is_multiple_of(d.every.max(1));
                self.phase = if long { Phase::LongBreak } else { Phase::ShortBreak };
                self.length = d.of(self.phase);
                self.run = Run::Running { ends: now + self.length };
                Some(Finished::Focus { long })
            }
            Phase::Timer => {
                self.stop();
                Some(Finished::Timer)
            }
            Phase::ShortBreak | Phase::LongBreak => {
                if self.phase == Phase::LongBreak {
                    self.done = 0;
                }
                self.stop();
                Some(Finished::Break)
            }
        }
    }

    /// Sleep until the displayed text changes (remaining time is rounded up, so it changes exactly at
    /// a second/minute multiple), or until the phase ends. `None` if not running.
    pub fn next_wake(&self, now: Instant, wake: Wake) -> Option<Duration> {
        let Run::Running { ends } = self.run else { return None };
        let left = ends.saturating_duration_since(now);
        let unit = match wake {
            Wake::Second => 1000,
            Wake::Minute => 60_000,
            Wake::End => return Some(left),
        };
        let ms = left.as_millis() as u64;
        let into = ms % unit;
        Some(Duration::from_millis(if into == 0 { unit } else { into }.min(ms)))
    }
}

/// Seconds shown, rounded up (0:01 until it is truly finished).
fn secs_up(left: Duration) -> u64 {
    left.as_millis().div_ceil(1000) as u64
}

/// "18:42".
pub fn clock(left: Duration) -> String {
    let s = secs_up(left);
    format!("{}:{:02}", s / 60, s % 60)
}

/// "19 min", for the compact notch that updates per minute.
pub fn minutes(left: Duration) -> String {
    format!("{} min", secs_up(left).div_ceil(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn focus_break_cycle_with_long_break() {
        let d = Durations { every: 2, ..Durations::default() };
        let mut p = Pomodoro::default();
        let t0 = Instant::now();
        assert_eq!(p.remaining(t0, &d), 25 * MIN);
        p.toggle(t0, &d);
        assert!(p.is_active());
        assert_eq!(p.tick(t0 + 24 * MIN, &d), None);
        let t1 = t0 + 25 * MIN;
        assert_eq!(p.tick(t1, &d), Some(Finished::Focus { long: false }));
        assert_eq!((p.phase(), p.done()), (Phase::ShortBreak, 1));
        assert_eq!(p.remaining(t1, &d), 5 * MIN);
        let t2 = t1 + 5 * MIN;
        assert_eq!(p.tick(t2, &d), Some(Finished::Break));
        assert!(!p.is_active());
        assert_eq!(p.phase(), Phase::Focus);
        // Second session: long break, then the session count returns to zero.
        p.toggle(t2, &d);
        let t3 = t2 + 25 * MIN;
        assert_eq!(p.tick(t3, &d), Some(Finished::Focus { long: true }));
        assert_eq!(p.remaining(t3, &d), 15 * MIN);
        assert_eq!(p.tick(t3 + 15 * MIN, &d), Some(Finished::Break));
        assert_eq!(p.done(), 0);
    }

    #[test]
    fn pause_freezes_the_countdown() {
        let d = Durations::default();
        let mut p = Pomodoro::default();
        let t0 = Instant::now();
        p.toggle(t0, &d);
        p.toggle(t0 + 10 * MIN, &d);
        assert!(p.is_paused());
        assert_eq!(p.remaining(t0 + 60 * MIN, &d), 15 * MIN);
        assert_eq!(p.tick(t0 + 60 * MIN, &d), None);
        assert_eq!(p.next_wake(t0, Wake::Second), None, "dijeda: tanpa timer sama sekali");
        p.toggle(t0 + 60 * MIN, &d);
        assert!((p.progress(t0 + 60 * MIN, &d) - 0.4).abs() < 1e-4);
        assert_eq!(p.tick(t0 + 75 * MIN, &d), Some(Finished::Focus { long: false }));
    }

    #[test]
    fn stop_returns_to_focus_and_keeps_count() {
        let d = Durations::default();
        let mut p = Pomodoro::default();
        let t0 = Instant::now();
        p.toggle(t0, &d);
        p.tick(t0 + 25 * MIN, &d);
        p.stop();
        assert!(!p.is_active());
        assert_eq!((p.phase(), p.done()), (Phase::Focus, 1));
        assert_eq!(p.progress(t0, &d), 0.0);
    }

    #[test]
    fn custom_focus_and_plain_timer() {
        let d = Durations::default();
        let mut p = Pomodoro::default();
        let t0 = Instant::now();
        p.start(t0, 50 * MIN, true);
        assert_eq!((p.phase(), p.remaining(t0, &d)), (Phase::Focus, 50 * MIN));
        // A focus session from the command bar is still followed by a break.
        assert_eq!(p.tick(t0 + 50 * MIN, &d), Some(Finished::Focus { long: false }));
        p.start(t0, 90 * Duration::from_secs(1), false);
        assert_eq!(p.phase(), Phase::Timer);
        assert_eq!(p.tick(t0 + 90 * Duration::from_secs(1), &d), Some(Finished::Timer));
        assert!(!p.is_active());
        assert_eq!((p.phase(), p.done()), (Phase::Focus, 1));
    }

    #[test]
    fn wakes_only_when_the_text_changes() {
        let d = Durations::default();
        let mut p = Pomodoro::default();
        let t0 = Instant::now();
        p.toggle(t0, &d);
        let now = t0 + Duration::from_millis(6 * 60_000 + 17_700); // 18:42.300 remaining
        assert_eq!(clock(p.remaining(now, &d)), "18:43");
        assert_eq!(minutes(p.remaining(now, &d)), "19 min");
        assert_eq!(p.next_wake(now, Wake::Second), Some(Duration::from_millis(300)));
        assert_eq!(p.next_wake(now, Wake::Minute), Some(Duration::from_millis(42_300)));
        assert_eq!(p.next_wake(now, Wake::End), Some(Duration::from_millis(18 * 60_000 + 42_300)));
        // Exactly at the boundary: wait one full unit.
        assert_eq!(p.next_wake(t0 + 5 * MIN, Wake::Minute), Some(MIN));
        // Remaining time is shorter than one unit.
        assert_eq!(p.next_wake(t0 + 25 * MIN - Duration::from_millis(400), Wake::Minute), Some(Duration::from_millis(400)));
    }

    #[test]
    fn labels() {
        assert_eq!(clock(Duration::from_millis(1)), "0:01");
        assert_eq!(clock(Duration::ZERO), "0:00");
        assert_eq!(clock(25 * MIN), "25:00");
        assert_eq!(minutes(Duration::from_secs(59)), "1 min");
        assert_eq!(minutes(25 * MIN), "25 min");
    }

    #[test]
    fn steps_stay_in_range() {
        let mut d = Durations::default();
        d.step(Length::Focus, true);
        assert_eq!(d.focus, 30);
        d.set(Length::Focus, 1000);
        assert_eq!(d.focus, 90);
        d.step(Length::Focus, true);
        assert_eq!(d.focus, 90);
        d.set(Length::Every, 0);
        assert_eq!(d.every, 2);
        d.step(Length::ShortBreak, false);
        assert_eq!(d.short_break, 4);
    }
}

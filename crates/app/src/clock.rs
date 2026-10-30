//! Calendar dates and task times; display labels are in English.

use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};

#[derive(Clone, Copy, Debug)]
pub struct Now {
    pub year: u16,
    pub weekday: u16, // 0 = Sunday
    pub day: u16,
    pub month: u16, // 1 = January
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millis: u16,
}

/// Local calendar date. Its ordering matches chronological order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub y: u16,
    pub m: u16,
    pub d: u16,
}

pub fn now() -> Now {
    let t = unsafe { GetLocalTime() };
    Now {
        year: t.wYear,
        weekday: t.wDayOfWeek,
        day: t.wDay,
        month: t.wMonth,
        hour: t.wHour,
        minute: t.wMinute,
        second: t.wSecond,
        millis: t.wMilliseconds,
    }
}

/// Current local-time offset from UTC (e.g. 7.0 for WIB), rounded to 15 minutes.
pub fn utc_offset_hours() -> f64 {
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let mut minutes = (local.wHour as i32 * 60 + local.wMinute as i32) - (utc.wHour as i32 * 60 + utc.wMinute as i32);
    let (l, u) = ((local.wYear, local.wMonth, local.wDay), (utc.wYear, utc.wMonth, utc.wDay));
    if l > u {
        minutes += 24 * 60;
    } else if l < u {
        minutes -= 24 * 60;
    }
    (minutes as f64 / 15.0).round() * 15.0 / 60.0
}

pub fn today() -> Date {
    now().date()
}

impl Now {
    pub fn date(&self) -> Date {
        Date { y: self.year, m: self.month, d: self.day }
    }
}

fn days_in_month(y: u16, m: u16) -> u16 {
    match m {
        2 if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Date {
    /// ISO format `YYYY-MM-DD`, also used in the task file.
    pub fn iso(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }

    pub fn prev(&self) -> Date {
        if self.d > 1 {
            Date { d: self.d - 1, ..*self }
        } else if self.m > 1 {
            Date { m: self.m - 1, d: days_in_month(self.y, self.m - 1), ..*self }
        } else {
            Date { y: self.y - 1, m: 12, d: 31 }
        }
    }

    pub fn next(&self) -> Date {
        if self.d < days_in_month(self.y, self.m) {
            Date { d: self.d + 1, ..*self }
        } else if self.m < 12 {
            Date { m: self.m + 1, d: 1, ..*self }
        } else {
            Date { y: self.y + 1, m: 1, d: 1 }
        }
    }

    /// `n` days earlier. Used for short spans (week, history limit), so looping is enough.
    pub fn back(&self, n: u32) -> Date {
        (0..n).fold(*self, |d, _| d.prev())
    }

    /// `n` days later.
    pub fn ahead(&self, n: u32) -> Date {
        (0..n).fold(*self, |d, _| d.next())
    }

    /// Hari dalam minggu, 0 = Minggu (algoritma Sakamoto).
    pub fn weekday(&self) -> u16 {
        const T: [u16; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let y = if self.m < 3 { self.y - 1 } else { self.y };
        (y + y / 4 - y / 100 + y / 400 + T[self.m as usize - 1] + self.d) % 7
    }

    /// Monday of this date's week; the week strip starts on Monday.
    pub fn monday(&self) -> Date {
        self.back(((self.weekday() + 6) % 7) as u32)
    }

    /// Display label, e.g. "Tue, Oct 28".
    pub fn label(&self) -> String {
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        format!("{}, {} {}", DAYS[self.weekday() as usize], MONTHS[(self.m as usize).clamp(1, 12) - 1], self.d)
    }
}

/// Task time (`HH:MM` 24-hour in the file) for display: "2:00 PM".
pub fn time12(hm: &str) -> String {
    let Some((h, m)) = hm.split_once(':').and_then(|(h, m)| Some((h.parse::<u16>().ok()?, m.parse::<u16>().ok()?))) else {
        return hm.to_string();
    };
    format!("{}:{m:02} {}", (h + 11) % 12 + 1, if h < 12 { "AM" } else { "PM" })
}

/// Delay until the next minute change (+ a small margin), so the clock only needs updating once per minute.
pub fn ms_to_next_minute(n: &Now) -> u32 {
    let elapsed = n.second as u32 * 1000 + n.millis as u32;
    60_000 - elapsed.min(59_999) + 50
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(weekday: u16, day: u16, month: u16, hour: u16, minute: u16, second: u16) -> Now {
        Now { year: 2026, weekday, day, month, hour, minute, second, millis: 0 }
    }

    #[test]
    fn labels() {
        assert_eq!(time12("10:00"), "10:00 AM");
        assert_eq!(time12("14:05"), "2:05 PM");
        assert_eq!(time12("00:30"), "12:30 AM");
        assert_eq!(time12("12:00"), "12:00 PM");
    }

    #[test]
    fn next_minute() {
        assert_eq!(ms_to_next_minute(&at(0, 1, 1, 0, 0, 0)), 60_050);
        assert_eq!(ms_to_next_minute(&at(0, 1, 1, 0, 0, 59)), 1_050);
    }

    #[test]
    fn dates() {
        let d = |y, m, d| Date { y, m, d };
        assert_eq!(d(2026, 10, 22).iso(), "2026-10-22");
        assert_eq!(d(2026, 10, 31).next(), d(2026, 11, 1));
        assert_eq!(d(2026, 12, 31).next(), d(2027, 1, 1));
        assert_eq!(d(2028, 2, 28).next(), d(2028, 2, 29));
        assert_eq!(d(2026, 11, 1).prev(), d(2026, 10, 31));
        assert_eq!(d(2027, 1, 1).prev(), d(2026, 12, 31));
        assert_eq!(d(2028, 3, 1).prev(), d(2028, 2, 29));
        assert!(d(2026, 9, 30) < d(2026, 10, 1));
    }

    #[test]
    fn weeks_and_labels() {
        let d = |y, m, d| Date { y, m, d };
        // 30 October 2026 is a Friday.
        assert_eq!(d(2026, 10, 30).weekday(), 5);
        assert_eq!(d(2026, 1, 1).weekday(), 4);
        assert_eq!(d(2028, 2, 29).weekday(), 2);
        assert_eq!(d(2026, 10, 30).monday(), d(2026, 10, 26));
        assert_eq!(d(2026, 10, 26).monday(), d(2026, 10, 26));
        assert_eq!(d(2026, 11, 1).monday(), d(2026, 10, 26));
        assert_eq!(d(2026, 10, 30).back(60), d(2026, 8, 31));
        assert_eq!(d(2026, 12, 29).ahead(7), d(2027, 1, 5));
        assert_eq!(d(2026, 10, 28).label(), "Wed, Oct 28");
        assert_eq!(d(2026, 11, 3).label(), "Tue, Nov 3");
    }
}

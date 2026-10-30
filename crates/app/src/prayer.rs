//! Prayer times are computed locally (no internet) with the Kemenag RI method: Fajr −20°, Isha −18°,
//! Asr per the Shafi'i school (shadow = object height), and 2 minutes of ihtiyat. The solar position formulas
//! follow PrayTimes.org. Times are computed in UTC and then shifted to the Windows time zone, so what
//! is shown is always the clock of the user's PC.

use crate::clock::Date;

/// Display names, in the order of `Times::prayers`.
pub const NAMES: [&str; 5] = ["Fajr", "Dhuhr", "Asr", "Maghrib", "Isha"];

const FAJR_ANGLE: f64 = 20.0;
const ISHA_ANGLE: f64 = 18.0;
/// Sunrise/sunset: the sun's upper limb touches the horizon, with refraction.
const HORIZON: f64 = 0.833;
/// Kemenag ihtiyat (minutes): prayers are delayed, sunrise is advanced. Dhuhr 3 minutes; matched against
/// the official schedule (see test `matches_kemenag`).
const IHTIYAT: f64 = 2.0;
const DHUHR_IHTIYAT: f64 = 3.0;

/// Location for the calculation.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// One day's times in minutes since local midnight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Times {
    /// Fajr, Dhuhr, Asr, Maghrib, Isha.
    pub prayers: [f64; 5],
    pub sunrise: f64,
}

fn rad(d: f64) -> f64 {
    d.to_radians()
}
fn dsin(d: f64) -> f64 {
    rad(d).sin()
}
fn dcos(d: f64) -> f64 {
    rad(d).cos()
}
fn fix(a: f64, b: f64) -> f64 {
    a - b * (a / b).floor()
}

fn julian(d: Date) -> f64 {
    let (mut y, mut m) = (d.y as f64, d.m as f64);
    if m <= 2.0 {
        y -= 1.0;
        m += 12.0;
    }
    let a = (y / 100.0).floor();
    let b = 2.0 - a + (a / 4.0).floor();
    (365.25 * (y + 4716.0)).floor() + (30.6001 * (m + 1.0)).floor() + d.d as f64 + b - 1524.5
}

/// Solar declination (degrees) and equation of time (hours) at Julian day `jd`.
fn sun(jd: f64) -> (f64, f64) {
    let d = jd - 2451545.0;
    let g = fix(357.529 + 0.98560028 * d, 360.0);
    let q = fix(280.459 + 0.98564736 * d, 360.0);
    let l = fix(q + 1.915 * dsin(g) + 0.020 * dsin(2.0 * g), 360.0);
    let e = 23.439 - 0.00000036 * d;
    let ra = fix((dcos(e) * dsin(l)).atan2(dcos(l)).to_degrees() / 15.0, 24.0);
    let eqt = q / 15.0 - ra;
    let decl = (dsin(e) * dsin(l)).asin().to_degrees();
    (decl, eqt)
}

struct Calc {
    lat: f64,
    jd: f64,
}

impl Calc {
    /// Solar noon (hours, UTC at longitude 0) for the estimate `t` (fraction of a day).
    fn noon(&self, t: f64) -> f64 {
        fix(12.0 - sun(self.jd + t).1, 24.0)
    }

    /// When the sun is `angle` degrees below the horizon, before (`before`) or after solar noon.
    fn at_angle(&self, angle: f64, t: f64, before: bool) -> f64 {
        let decl = sun(self.jd + t).0;
        let c = (-dsin(angle) - dsin(decl) * dsin(self.lat)) / (dcos(decl) * dcos(self.lat));
        if !(-1.0..=1.0).contains(&c) {
            return f64::NAN;
        }
        let span = c.acos().to_degrees() / 15.0;
        self.noon(t) + if before { -span } else { span }
    }

    /// Asr: shadow equals the object's height plus its shadow at noon.
    fn asr(&self, t: f64) -> f64 {
        let decl = sun(self.jd + t).0;
        let angle = -(1.0 / (1.0 + rad((self.lat - decl).abs()).tan())).atan().to_degrees();
        self.at_angle(angle, t, false)
    }
}

/// Schedule for `date` at `lat`/`lon`, in time zone `tz` (hours from UTC).
pub fn times(date: Date, lat: f64, lon: f64, tz: f64) -> Times {
    let c = Calc { lat, jd: julian(date) - lon / (15.0 * 24.0) };
    // Initial estimate, then refined twice with the sun's position at the more accurate time.
    let mut t = [5.0, 6.0, 12.0, 13.0, 18.0, 18.0];
    for _ in 0..2 {
        let p = t.map(|h| h / 24.0);
        t = [
            c.at_angle(FAJR_ANGLE, p[0], true),
            c.at_angle(HORIZON, p[1], true),
            c.noon(p[2]),
            c.asr(p[3]),
            c.at_angle(HORIZON, p[4], false),
            c.at_angle(ISHA_ANGLE, p[5], false),
        ];
    }
    let [mut fajr, sunrise, dhuhr, asr, sunset, mut isha] = t.map(|h| h + tz - lon / 15.0);
    // High latitude: if Fajr/Isha is undefined or too far off, use a fraction of the night
    // proportional to its angle ("angle based" method).
    let night = fix(sunrise - sunset, 24.0);
    let fajr_max = FAJR_ANGLE / 60.0 * night;
    if fajr.is_nan() || fix(sunrise - fajr, 24.0) > fajr_max {
        fajr = sunrise - fajr_max;
    }
    let isha_max = ISHA_ANGLE / 60.0 * night;
    if isha.is_nan() || fix(isha - sunset, 24.0) > isha_max {
        isha = sunset + isha_max;
    }
    let min = |h: f64, adj: f64| fix(h * 60.0 + adj, 24.0 * 60.0);
    Times {
        prayers: [min(fajr, IHTIYAT), min(dhuhr, DHUHR_IHTIYAT), min(asr, IHTIYAT), min(sunset, IHTIYAT), min(isha, IHTIYAT)],
        sunrise: min(sunrise, -IHTIYAT),
    }
}

/// "04:21" from minutes since midnight (rounded up, like the official schedule).
pub fn hm(minutes: f64) -> String {
    let m = minutes.ceil() as u32 % (24 * 60);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// The next prayer after `now` (local minutes): (index in `NAMES`, minutes left). After Isha,
/// tomorrow's Fajr (computed from today's schedule; the day-to-day difference is only a few seconds).
pub fn next(t: &Times, now: f64) -> (usize, f64) {
    for (i, p) in t.prayers.iter().enumerate() {
        let at = p.ceil();
        if at > now {
            return (i, at - now);
        }
    }
    (0, t.prayers[0].ceil() + 24.0 * 60.0 - now)
}

/// The on-time reminder stays visible until dismissed, but for at most this long (minutes),
/// so it doesn't stick forever if the user is away from the PC.
pub const REMINDER_HOLD_MIN: u32 = 15;

/// The reminder due exactly at minute `now` (local minutes, whole): (index into `NAMES`, before
/// the time?). Once 10 minutes before, once more exactly at the time.
pub fn alert_at(t: &Times, now: f64) -> Option<(usize, bool)> {
    t.prayers.iter().enumerate().find_map(|(i, p)| {
        let at = p.ceil();
        if now == at - 10.0 {
            Some((i, true))
        } else if now == at {
            Some((i, false))
        } else {
            None
        }
    })
}

/// On-time reminder text: (small caption, title). While the adhan plays, the caption
/// says that clicking the notch mutes it.
pub fn reminder_text(i: usize, at: f64, adhan: bool) -> (String, String) {
    let kicker = if adhan { "Adhan playing \u{00B7} click to mute" } else { "It's time to pray" };
    (kicker.to_string(), format!("{} \u{00B7} {}", NAMES[i], hm(at)))
}

const HIJRI_MONTHS: [&str; 12] =
    ["Muharram", "Safar", "Rabi I", "Rabi II", "Jumada I", "Jumada II", "Rajab", "Sha'ban", "Ramadan", "Shawwal", "Dhu al-Qa'dah", "Dhu al-Hijjah"];

/// Hijri date per the tabular calendar (Kuwaiti algorithm), e.g. "12 Jumada I 1448". May differ by
/// one day from the official determination, which uses moon sighting.
pub fn hijri(d: Date) -> String {
    let (y, m, day) = (d.y as i64, d.m as i64, d.d as i64);
    let a = (14 - m) / 12;
    let (yy, mm) = (y + 4800 - a, m + 12 * a - 3);
    let jd = day + (153 * mm + 2) / 5 + 365 * yy + yy / 4 - yy / 100 + yy / 400 - 32045;
    let l = jd - 1948440 + 10632;
    let n = (l - 1) / 10631;
    let l = l - 10631 * n + 354;
    let j = ((10985 - l) / 5316) * ((50 * l) / 17719) + (l / 5670) * ((43 * l) / 15238);
    let l = l - ((30 - j) / 15) * ((17719 * j) / 50) - (j / 16) * ((15238 * j) / 43) + 29;
    let month = (24 * l) / 709;
    let hd = l - (709 * month) / 24;
    let hy = 30 * n + j - 30;
    format!("{hd} {} {hy}", HIJRI_MONTHS[(month.clamp(1, 12) - 1) as usize])
}

/// Built-in cities: name, latitude, longitude. Enough to look up without internet; a difference of a few
/// kilometers only shifts the schedule by a few seconds.
pub const CITIES: &[(&str, f64, f64)] = &[
    // Indonesia
    ("Jakarta", -6.2088, 106.8456),
    ("Bogor", -6.5971, 106.8060),
    ("Depok", -6.4025, 106.7942),
    ("Tangerang", -6.1783, 106.6319),
    ("Bekasi", -6.2383, 106.9756),
    ("Serang", -6.1200, 106.1503),
    ("Bandung", -6.9175, 107.6191),
    ("Cirebon", -6.7320, 108.5523),
    ("Sukabumi", -6.9277, 106.9300),
    ("Karawang", -6.3227, 107.3376),
    ("Tasikmalaya", -7.3274, 108.2207),
    ("Semarang", -6.9667, 110.4167),
    ("Surakarta (Solo)", -7.5755, 110.8243),
    ("Yogyakarta", -7.7956, 110.3695),
    ("Magelang", -7.4797, 110.2177),
    ("Purwokerto", -7.4245, 109.2302),
    ("Tegal", -6.8694, 109.1402),
    ("Pekalongan", -6.8898, 109.6746),
    ("Surabaya", -7.2575, 112.7521),
    ("Malang", -7.9666, 112.6326),
    ("Kediri", -7.8480, 112.0178),
    ("Madiun", -7.6298, 111.5239),
    ("Probolinggo", -7.7543, 113.2159),
    ("Jember", -8.1845, 113.6681),
    ("Banyuwangi", -8.2192, 114.3691),
    ("Denpasar", -8.6705, 115.2126),
    ("Mataram", -8.5833, 116.1167),
    ("Kupang", -10.1772, 123.6070),
    ("Banda Aceh", 5.5483, 95.3238),
    ("Medan", 3.5952, 98.6722),
    ("Padang", -0.9471, 100.4172),
    ("Bukittinggi", -0.3039, 100.3696),
    ("Pekanbaru", 0.5071, 101.4478),
    ("Batam", 1.0456, 104.0305),
    ("Tanjung Pinang", 0.9186, 104.4554),
    ("Jambi", -1.6101, 103.6131),
    ("Palembang", -2.9761, 104.7754),
    ("Bengkulu", -3.8004, 102.2655),
    ("Bandar Lampung", -5.3971, 105.2668),
    ("Pangkal Pinang", -2.1316, 106.1169),
    ("Pontianak", -0.0263, 109.3425),
    ("Palangka Raya", -2.2161, 113.9135),
    ("Banjarmasin", -3.3186, 114.5944),
    ("Samarinda", -0.5022, 117.1536),
    ("Balikpapan", -1.2379, 116.8529),
    ("Tanjung Selor", 2.8375, 117.3653),
    ("Manado", 1.4748, 124.8421),
    ("Gorontalo", 0.5435, 123.0568),
    ("Palu", -0.8917, 119.8707),
    ("Mamuju", -2.6748, 118.8885),
    ("Makassar", -5.1477, 119.4327),
    ("Kendari", -3.9985, 122.5129),
    ("Ambon", -3.6954, 128.1814),
    ("Ternate", 0.7893, 127.3753),
    ("Sofifi", 0.7370, 127.5560),
    ("Jayapura", -2.5337, 140.7181),
    ("Manokwari", -0.8615, 134.0620),
    ("Sorong", -0.8762, 131.2558),
    ("Merauke", -8.4932, 140.4018),
    // Dunia
    ("Mecca", 21.3891, 39.8579),
    ("Medina", 24.5247, 39.5692),
    ("Jeddah", 21.4858, 39.1925),
    ("Riyadh", 24.7136, 46.6753),
    ("Kuala Lumpur", 3.1390, 101.6869),
    ("Singapore", 1.3521, 103.8198),
    ("Bandar Seri Begawan", 4.9031, 114.9398),
    ("Bangkok", 13.7563, 100.5018),
    ("Manila", 14.5995, 120.9842),
    ("Hong Kong", 22.3193, 114.1694),
    ("Beijing", 39.9042, 116.4074),
    ("Seoul", 37.5665, 126.9780),
    ("Tokyo", 35.6762, 139.6503),
    ("Sydney", -33.8688, 151.2093),
    ("Melbourne", -37.8136, 144.9631),
    ("Perth", -31.9523, 115.8613),
    ("Dubai", 25.2048, 55.2708),
    ("Abu Dhabi", 24.4539, 54.3773),
    ("Doha", 25.2854, 51.5310),
    ("Kuwait City", 29.3759, 47.9774),
    ("Amman", 31.9454, 35.9284),
    ("Cairo", 30.0444, 31.2357),
    ("Istanbul", 41.0082, 28.9784),
    ("Tehran", 35.6892, 51.3890),
    ("Karachi", 24.8607, 67.0011),
    ("Delhi", 28.6139, 77.2090),
    ("Mumbai", 19.0760, 72.8777),
    ("Dhaka", 23.8103, 90.4125),
    ("Casablanca", 33.5731, -7.5898),
    ("London", 51.5072, -0.1276),
    ("Paris", 48.8566, 2.3522),
    ("Amsterdam", 52.3676, 4.9041),
    ("Berlin", 52.5200, 13.4050),
    ("New York", 40.7128, -74.0060),
    ("Toronto", 43.6532, -79.3832),
    ("Los Angeles", 34.0522, -118.2437),
    ("San Francisco", 37.7749, -122.4194),
];

/// Cities whose name starts with (listed first) or contains `q`, case-insensitive.
pub fn search(q: &str) -> Vec<Place> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let place = |&(name, lat, lon): &(&str, f64, f64)| Place { name: name.to_string(), lat, lon };
    let mut out: Vec<Place> = CITIES.iter().filter(|c| c.0.to_lowercase().starts_with(&q)).map(place).collect();
    out.extend(CITIES.iter().filter(|c| !c.0.to_lowercase().starts_with(&q) && c.0.to_lowercase().contains(&q)).map(place));
    out
}

/// Nearest built-in city and its distance (km), to name the location from Windows.
pub fn nearest(lat: f64, lon: f64) -> (&'static str, f64) {
    let km = |a: f64, b: f64| {
        let (p1, p2) = (rad(lat), rad(a));
        let h = (rad(a - lat) / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (rad(b - lon) / 2.0).sin().powi(2);
        6371.0 * 2.0 * h.sqrt().asin()
    };
    CITIES.iter().map(|&(n, a, b)| (n, km(a, b))).min_by(|x, y| x.1.total_cmp(&y.1)).unwrap_or(("", 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: u16, m: u16, d: u16) -> Date {
        Date { y, m, d }
    }

    #[test]
    fn order_and_range() {
        let t = times(date(2026, 10, 24), -6.2088, 106.8456, 7.0);
        let p = t.prayers;
        assert!(p[0] < t.sunrise && t.sunrise < p[1] && p[1] < p[2] && p[2] < p[3] && p[3] < p[4], "{t:?}");
        // Jakarta: Dhuhr around 11:40–12:00 all year.
        assert!((11.0 * 60.0..12.0 * 60.0 + 5.0).contains(&p[1]), "{}", hm(p[1]));
    }

    #[test]
    fn matches_kemenag() {
        // Official Kemenag schedule for 24 October 2026 (bimasislam, via api.myquran.com).
        for (lat, lon, tz, official) in [
            (-6.2088, 106.8456, 7.0, ["04:12", "11:40", "14:51", "17:49", "18:59"]),
            (-5.1477, 119.4327, 8.0, ["04:22", "11:50", "15:02", "17:57", "19:08"]),
        ] {
            let t = times(date(2026, 10, 24), lat, lon, tz);
            for (ours, theirs) in t.prayers.iter().zip(official) {
                let (h, m) = theirs.split_once(':').unwrap();
                let theirs = h.parse::<f64>().unwrap() * 60.0 + m.parse::<f64>().unwrap();
                assert!((ours.ceil() - theirs).abs() <= 1.0, "{} vs {theirs}", hm(*ours));
            }
        }
    }

    #[test]
    fn high_latitude_still_has_fajr_and_isha() {
        // London in midsummer: the 18–20° angles are never reached.
        let t = times(date(2026, 6, 21), 51.5072, -0.1276, 1.0);
        assert!(t.prayers.iter().all(|m| m.is_finite()));
        assert!(t.prayers[0] < t.sunrise && t.prayers[4] > t.prayers[3]);
    }

    #[test]
    fn next_prayer_wraps_to_tomorrow() {
        let t = Times { prayers: [270.0, 720.0, 915.0, 1080.0, 1155.0], sunrise: 330.0 };
        assert_eq!(next(&t, 600.0), (1, 120.0));
        assert_eq!(next(&t, 1200.0), (0, 270.0 + 1440.0 - 1200.0));
        assert_eq!(hm(1155.2), "19:16");
    }

    #[test]
    fn alerts_ten_minutes_before_and_on_time() {
        let t = Times { prayers: [270.0, 719.4, 915.0, 1080.0, 1155.0], sunrise: 330.0 };
        // Dhuhr 11:59.4 is rounded to 12:00 like the displayed schedule.
        assert_eq!(alert_at(&t, 710.0), Some((1, true)));
        assert_eq!(alert_at(&t, 720.0), Some((1, false)));
        assert_eq!(alert_at(&t, 1080.0), Some((3, false)));
        assert_eq!(alert_at(&t, 1070.0), Some((3, true)));
        assert_eq!(alert_at(&t, 715.0), None);
        assert_eq!(alert_at(&t, 0.0), None);
    }

    #[test]
    fn reminder_text_names_prayer_and_time() {
        let (kicker, title) = reminder_text(3, 1079.3, false);
        assert_eq!(title, "Maghrib \u{00B7} 18:00");
        assert_eq!(kicker, "It's time to pray");
        assert!(reminder_text(3, 1079.3, true).0.contains("mute"));
    }

    #[test]
    fn hijri_dates() {
        // Ramadan 1446 start and Eid al-Adha 1445 (Indonesia).
        assert_eq!(hijri(date(2025, 3, 1)), "1 Ramadan 1446");
        assert_eq!(hijri(date(2024, 6, 17)), "10 Dhu al-Hijjah 1445");
        assert_eq!(hijri(date(2026, 10, 24)), "12 Jumada I 1448");
    }

    #[test]
    fn city_search_and_nearest() {
        assert_eq!(search("band")[0].name, "Bandung");
        assert!(search("lumpur").iter().any(|p| p.name == "Kuala Lumpur"));
        assert!(search("  ").is_empty());
        let (name, km) = nearest(-6.90, 107.60);
        assert_eq!(name, "Bandung");
        assert!(km < 5.0);
    }
}


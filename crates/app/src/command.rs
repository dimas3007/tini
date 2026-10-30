//! Command bar: understands text typed into the notch. Pure (no Win32) so it is easy to test.
//!
//! Order: leading duration = timer ("25m review PR"), calculation ("12*8+5"), "claude" prefix =
//! ask Claude Code, "g" / "?" prefix = web search. Otherwise the text is matched against apps and
//! folders, and can still be saved as a task.

/// Kinds of things that can be opened from the command bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Start Menu shortcut.
    App,
    Folder,
    /// Project folder where an agent works; opened in VS Code.
    Project,
}

impl Kind {
    pub fn code(self) -> char {
        match self {
            Kind::App => 'a',
            Kind::Folder => 'f',
            Kind::Project => 'p',
        }
    }

    pub fn from_code(c: &str) -> Option<Kind> {
        match c {
            "a" => Some(Kind::App),
            "f" => Some(Kind::Folder),
            "p" => Some(Kind::Project),
            _ => None,
        }
    }
}

/// App, folder, or project that can be searched and pinned. `target`: the path to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub target: String,
}

/// Project name from its folder path.
pub fn folder_name(path: &str) -> String {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Intent of the typed text.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    /// Count down `secs` seconds; `label` is the rest of the text (the task being worked on).
    Timer { secs: u64, label: String },
    /// Calculation result and its expression tidied up for display.
    Calc { value: f64, expr: String },
    Claude { prompt: String },
    Web { query: String },
    /// Picks the city for the prayer schedule: "city bandung", "kota medan", "prayer".
    City { query: String },
    /// Not a command: search apps/folders, or save as a task.
    Text,
}

pub fn intent(text: &str) -> Intent {
    let t = text.trim();
    if t.is_empty() {
        return Intent::Text;
    }
    if let Some((secs, label)) = timer(t) {
        return Intent::Timer { secs, label };
    }
    if let Some((value, expr)) = calc(t) {
        return Intent::Calc { value, expr };
    }
    let lower = t.to_lowercase();
    for prefix in ["claude ", "ask claude ", "/claude "] {
        if lower.starts_with(prefix) && t.len() > prefix.len() {
            return Intent::Claude { prompt: t[prefix.len()..].trim().to_string() };
        }
    }
    for word in ["city", "kota", "prayer", "sholat", "shalat"] {
        if lower == word {
            return Intent::City { query: String::new() };
        }
        if let Some(rest) = lower.strip_prefix(word).and_then(|r| r.strip_prefix(' ')) {
            return Intent::City { query: rest.trim().to_string() };
        }
    }
    for prefix in ["g ", "? ", "search "] {
        if lower.starts_with(prefix) && t.len() > prefix.len() {
            return Intent::Web { query: t[prefix.len()..].trim().to_string() };
        }
    }
    Intent::Text
}

/// Time unit in seconds from its suffix: m, min, menit, h, jam, s, detik.
fn unit(s: &str) -> Option<u64> {
    match s {
        "s" | "sec" | "secs" | "detik" => Some(1),
        "m" | "min" | "mins" | "minute" | "minutes" | "menit" => Some(60),
        "h" | "hr" | "hrs" | "hour" | "hours" | "jam" => Some(3600),
        _ => None,
    }
}

/// Duration in one token: "25m", "1h30m", "90s", "1.5h". A number without a unit isn't a duration.
pub fn duration(token: &str) -> Option<u64> {
    let t = token.to_lowercase();
    let mut total = 0.0;
    let mut rest = t.as_str();
    let mut parts = 0;
    while !rest.is_empty() {
        let num_len = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        if num_len == 0 {
            return None;
        }
        let n: f64 = rest[..num_len].parse().ok()?;
        rest = &rest[num_len..];
        let unit_len = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        let secs = unit(&rest[..unit_len])?;
        rest = &rest[unit_len..];
        total += n * secs as f64;
        parts += 1;
    }
    let secs = total.round() as u64;
    (parts > 0 && secs > 0 && secs <= 24 * 3600).then_some(secs)
}

/// "25m review PR" or "25 min review PR" → (1500, "review PR").
fn timer(t: &str) -> Option<(u64, String)> {
    let mut words = t.split_whitespace();
    let first = words.next()?;
    if let Some(secs) = duration(first) {
        return Some((secs, words.collect::<Vec<_>>().join(" ")));
    }
    // Number and unit separated: "25 min".
    let n: f64 = first.parse().ok()?;
    let secs = unit(&words.next()?.to_lowercase())?;
    let total = (n * secs as f64).round() as u64;
    (total > 0 && total <= 24 * 3600).then(|| (total, words.collect::<Vec<_>>().join(" ")))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Op(char),
    Percent,
    Open,
    Close,
}

/// Number with thousands separators "1,250,000" / "1.250.000", or decimal "2,5" / "2.5".
fn number(s: &str) -> Option<f64> {
    let commas = s.matches(',').count();
    let dots = s.matches('.').count();
    let thousands = |sep: char| s.split(sep).skip(1).all(|g| g.len() == 3 && g.chars().all(|c| c.is_ascii_digit()));
    let normal = if commas > 0 && dots == 0 && (commas > 1 || thousands(',')) {
        s.replace(',', "")
    } else if commas == 1 && dots == 0 {
        s.replace(',', ".")
    } else if dots > 1 && commas == 0 && thousands('.') {
        s.replace('.', "")
    } else if commas > 0 && dots == 1 && s.find(',') < s.find('.') {
        // "1,250.75"
        s.replace(',', "")
    } else {
        s.to_string()
    };
    normal.parse().ok()
}

fn tokens(s: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '_' => i += 1,
            '0'..='9' | '.' | ',' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || matches!(chars[i], '.' | ',')) {
                    i += 1;
                }
                out.push(Tok::Num(number(&chars[start..i].iter().collect::<String>())?));
            }
            '+' | '-' | '^' => {
                out.push(Tok::Op(c));
                i += 1;
            }
            '*' | 'x' | 'X' | '×' => {
                out.push(Tok::Op('*'));
                i += 1;
            }
            '/' | '÷' => {
                out.push(Tok::Op('/'));
                i += 1;
            }
            '%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            '(' => {
                out.push(Tok::Open);
                i += 1;
            }
            ')' => {
                out.push(Tok::Close);
                i += 1;
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Small recursive descent. Percent after + / − means percent of the left side ("200 + 10%" = 220),
/// otherwise it is divided by a hundred ("1000 * 11%" = 110).
struct Calc {
    toks: Vec<Tok>,
    at: usize,
}

impl Calc {
    fn peek(&self) -> Option<Tok> {
        self.toks.get(self.at).copied()
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?.0;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek() {
            self.at += 1;
            let (r, percent) = self.term()?;
            let r = if percent { v * r } else { r };
            v = if op == '+' { v + r } else { v - r };
        }
        Some(v)
    }

    /// (value, whether it is a single percent factor only).
    fn term(&mut self) -> Option<(f64, bool)> {
        let (mut v, mut percent) = self.power()?;
        while let Some(Tok::Op(op @ ('*' | '/'))) = self.peek() {
            self.at += 1;
            let r = self.power()?.0;
            v = if op == '*' { v * r } else { v / r };
            percent = false;
        }
        Some((v, percent))
    }

    fn power(&mut self) -> Option<(f64, bool)> {
        let (base, percent) = self.unary()?;
        if self.peek() == Some(Tok::Op('^')) {
            self.at += 1;
            return Some((base.powf(self.power()?.0), false));
        }
        Some((base, percent))
    }

    fn unary(&mut self) -> Option<(f64, bool)> {
        if self.peek() == Some(Tok::Op('-')) {
            self.at += 1;
            return self.unary().map(|(v, p)| (-v, p));
        }
        let mut v = match self.peek()? {
            Tok::Num(n) => {
                self.at += 1;
                n
            }
            Tok::Open => {
                self.at += 1;
                let v = self.expr()?;
                if self.peek() != Some(Tok::Close) {
                    return None;
                }
                self.at += 1;
                v
            }
            _ => return None,
        };
        let mut percent = false;
        if self.peek() == Some(Tok::Percent) {
            self.at += 1;
            v /= 100.0;
            percent = true;
        }
        Some((v, percent))
    }
}

/// Simple calculation. A lone number (e.g. "2025") isn't a calculation.
pub fn calc(text: &str) -> Option<(f64, String)> {
    // A date like "2026-10-23" isn't a subtraction.
    let parts: Vec<&str> = text.trim().split('-').collect();
    if parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.len() <= 4 && p.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let toks = tokens(text)?;
    let ops = toks.iter().filter(|t| matches!(t, Tok::Op(_) | Tok::Percent)).count();
    let nums = toks.iter().filter(|t| matches!(t, Tok::Num(_))).count();
    if nums == 0 || ops == 0 || (nums == 1 && !toks.contains(&Tok::Percent)) {
        return None;
    }
    let mut c = Calc { toks: toks.clone(), at: 0 };
    let v = c.expr()?;
    if c.at != toks.len() || !v.is_finite() || v.abs() >= 1e15 {
        return None;
    }
    Some((v, pretty(&toks)))
}

/// Tidied expression: "1250000*12*11%" → "1,250,000 × 12 × 11%".
fn pretty(toks: &[Tok]) -> String {
    let mut s = String::new();
    for (i, t) in toks.iter().enumerate() {
        let unary = i == 0 || matches!(toks[i - 1], Tok::Op(_) | Tok::Open);
        match t {
            Tok::Num(n) => s.push_str(&format_number(*n)),
            Tok::Op('-') if unary => s.push('-'),
            Tok::Op(op) => {
                let sym = match op {
                    '*' => '×',
                    '/' => '÷',
                    '-' => '−',
                    o => *o,
                };
                s.push_str(&format!(" {sym} "));
            }
            Tok::Percent => s.push('%'),
            Tok::Open => s.push('('),
            Tok::Close => s.push(')'),
        }
    }
    s
}

/// Number with thousands separators and at most six decimals: 1650000 → "1,650,000".
pub fn format_number(v: f64) -> String {
    let v = (v * 1e6).round() / 1e6;
    let neg = v < 0.0;
    let abs = v.abs();
    let int = abs.trunc() as u64;
    let mut frac = format!("{:.6}", abs.fract());
    frac = frac.trim_start_matches('0').trim_end_matches('0').trim_end_matches('.').to_string();
    let digits = int.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{}{grouped}{frac}", if neg { "-" } else { "" })
}

/// Calculation result for copying: no thousands separators, so it can be pasted into other apps.
pub fn plain_number(v: f64) -> String {
    format_number(v).replace(',', "")
}

/// How well `name` matches `query` (all lowercase): 0 = no match. Start of the name is
/// strongest, then start of a word, then the initials of each word ("vsc" → Visual Studio Code), then the middle.
pub fn score(name: &str, query: &str) -> u32 {
    let (name, query) = (name.to_lowercase(), query.trim().to_lowercase());
    if query.is_empty() {
        return 0;
    }
    if name.starts_with(&query) {
        return 100 - (name.len() - query.len()).min(40) as u32;
    }
    let words: Vec<&str> = name.split([' ', '-', '_', '.']).filter(|w| !w.is_empty()).collect();
    if words.iter().any(|w| w.starts_with(&query)) {
        return 60;
    }
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    if query.len() >= 2 && initials.starts_with(&query) {
        return 55;
    }
    if query.len() >= 3 && name.contains(&query) {
        return 30;
    }
    0
}

/// Encodes text for a URL query.
pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Single-line http(s) link: (host without "www.", remaining readable path).
pub fn link(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    if t.len() > 2048 || t.contains(char::is_whitespace) {
        return None;
    }
    let rest = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    let host = host.trim_start_matches("www.").to_string();
    // Most meaningful part of the path: the last non-empty segment, without query.
    let path = path.split(['?', '#']).next().unwrap_or("");
    let seg = path.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>();
    let title = match seg.as_slice() {
        [] => host.clone(),
        [.., a, b] if b.chars().all(|c| c.is_ascii_digit()) => format!("{a} {b}"),
        [.., last] => last.to_string(),
    };
    let title = percent_decode(&title).replace(['-', '_'], " ");
    Some((host, title))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            // Checked on bytes: a "%" followed by a non-ASCII letter must not cut a character.
            if let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// "25:00", "1:30:00".
pub fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn percent_before_non_ascii_does_not_crash() {
        // "%" followed by a multi-byte character: used to cut the character in the middle and make the app exit.
        assert!(link("https://x.com/promo-50%\u{2013}off").is_some());
        assert!(link("https://example.com/100%\u{65E5}\u{672C}").is_some());
        assert!(link("https://example.com/a%2").is_some());
    }

    use super::*;

    #[test]
    fn timers() {
        assert_eq!(intent("25m review PR"), Intent::Timer { secs: 1500, label: "review PR".into() });
        assert_eq!(intent("1h30m"), Intent::Timer { secs: 5400, label: String::new() });
        assert_eq!(intent("90s tea"), Intent::Timer { secs: 90, label: "tea".into() });
        assert_eq!(intent("10 menit istirahat"), Intent::Timer { secs: 600, label: "istirahat".into() });
        assert_eq!(intent("1.5h"), Intent::Timer { secs: 5400, label: String::new() });
        // A number without a unit, or a word that happens to start with a number, isn't a timer.
        assert_eq!(intent("2 tasks"), Intent::Text);
        assert_eq!(intent("3d model"), Intent::Text);
        assert_eq!(duration("48h"), None);
    }

    #[test]
    fn sums() {
        let v = |t: &str| calc(t).map(|(v, _)| v);
        assert_eq!(v("12*8+5"), Some(101.0));
        assert_eq!(v("1250000 * 12 * 11%"), Some(1_650_000.0));
        assert_eq!(v("200 + 10%"), Some(220.0));
        assert_eq!(v("200 - 10%"), Some(180.0));
        assert_eq!(v("(2+3)^2"), Some(25.0));
        assert_eq!(v("1.250.000 / 4"), Some(312_500.0));
        assert_eq!(v("2,5 x 4"), Some(10.0));
        assert_eq!(v("-3 * -3"), Some(9.0));
        assert_eq!(v("15%"), Some(0.15));
        assert_eq!(v("2025"), None);
        assert_eq!(v("1/0"), None);
        assert_eq!(v("call mom"), None);
        assert_eq!(v("2026-10-23"), None);
        assert_eq!(v("10:30"), None);
        assert_eq!(calc("1250000*12*11%").unwrap().1, "1,250,000 × 12 × 11%");
        assert_eq!(format_number(1650000.0), "1,650,000");
        assert_eq!(format_number(-0.125), "-0.125");
        assert_eq!(format_number(2.0 / 3.0), "0.666667");
        assert_eq!(plain_number(1650000.5), "1650000.5");
    }

    #[test]
    fn prefixes() {
        assert_eq!(intent("claude tidy up the README"), Intent::Claude { prompt: "tidy up the README".into() });
        assert_eq!(intent("g rust lifetimes"), Intent::Web { query: "rust lifetimes".into() });
        assert_eq!(intent("vs"), Intent::Text);
        assert_eq!(intent("beli susu besok 9"), Intent::Text);
    }

    #[test]
    fn matching() {
        assert!(score("Visual Studio Code", "vis") > score("Visual Studio Code", "code"));
        assert!(score("Visual Studio Code", "code") > 0);
        assert!(score("Visual Studio Code", "vsc") > 0);
        assert_eq!(score("Visual Studio Code", "xyz"), 0);
        assert_eq!(score("Notepad", "pad"), 30);
        assert!(score("Terminal", "term") > score("Windows Terminal", "term"));
    }

    #[test]
    fn links() {
        assert_eq!(link("https://github.com/acme/notch/pull/142"), Some(("github.com".into(), "pull 142".into())));
        assert_eq!(link("https://www.example.com"), Some(("example.com".into(), "example.com".into())));
        assert_eq!(link("https://docs.rs/windows/latest/fix-race_in%20card?x=1"), Some(("docs.rs".into(), "fix race in card".into())));
        assert_eq!(link("not a link"), None);
        assert_eq!(link("https://localhost"), None);
        assert_eq!(url_encode("a b&c"), "a+b%26c");
        assert_eq!(clock(1500), "25:00");
        assert_eq!(clock(5400), "1:30:00");
    }
}

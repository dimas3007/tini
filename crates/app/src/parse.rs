//! Simple sentence splitter without AI: one typed or dictated input → several tasks, complete with
//! times ("jam 2", "jam setengah tiga sore", "14.30", "3pm") and "besok" (tomorrow).
//!
//! Example: "hari ini aku mau review PR, terus jam dua ada meeting sama tim, sama jangan lupa bales email"
//! → "Review PR" · "Meeting sama tim" (14:00) · "Bales email".

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub title: String,
    pub time: Option<(u8, u8)>,
    pub tomorrow: bool,
}

/// Connector words that separate tasks. "dan"/"sama" (and/with) are excluded because they often appear inside one task
/// ("review dan merge PR", "meeting sama tim"); they are only dropped when at the start of a chunk.
const CONNECTORS: &[&[&str]] = &[
    &["habis", "itu"],
    &["abis", "itu"],
    &["setelah", "itu"],
    &["and", "then"],
    &["terus"],
    &["trus"],
    &["lalu"],
    &["kemudian"],
    &["then"],
];

const LEADING_FILLERS: &[&[&str]] = &[
    &["hari", "ini"],
    &["jangan", "lupa"],
    &["jgn", "lupa"],
    &["need", "to"],
    &["have", "to"],
    &["aku"],
    &["saya"],
    &["gue"],
    &["gw"],
    &["mau"],
    &["harus"],
    &["perlu"],
    &["akan"],
    &["ingin"],
    &["sama"],
    &["dan"],
    &["and"],
    &["juga"],
    &["tolong"],
    &["nanti"],
    &["ada"],
    &["today"],
    &["i"],
    &["to"],
];

const TRAILING_FILLERS: &[&[&str]] = &[&["hari", "ini"], &["ya"], &["yah"], &["deh"], &["nih"], &["dong"], &["juga"], &["today"]];

const TIME_MARKERS: &[&str] = &["jam", "jm", "pukul", "at"];

fn number_word(w: &str) -> Option<u8> {
    Some(match w {
        "satu" => 1,
        "dua" => 2,
        "tiga" => 3,
        "empat" => 4,
        "lima" => 5,
        "enam" => 6,
        "tujuh" => 7,
        "delapan" => 8,
        "sembilan" => 9,
        "sepuluh" => 10,
        "sebelas" => 11,
        _ => return None,
    })
}

pub fn parse(input: &str) -> Vec<Parsed> {
    chunks(input)
        .iter()
        .flat_map(|chunk| split_on_connectors(chunk.split_whitespace().map(str::to_string).collect()))
        .filter_map(parse_piece)
        .collect()
}

/// Splits on `, ; ! ?` and newlines, plus periods that end a sentence (not "14.30").
fn chunks(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = vec![String::new()];
    for (i, &c) in chars.iter().enumerate() {
        let decimal_point = c == '.'
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        if matches!(c, ',' | ';' | '!' | '?' | '\n' | '\r') || (c == '.' && !decimal_point) {
            out.push(String::new());
        } else {
            out.last_mut().unwrap().push(c);
        }
    }
    out.retain(|s| !s.trim().is_empty());
    out
}

fn lower(tokens: &[String]) -> Vec<String> {
    tokens.iter().map(|t| t.to_lowercase()).collect()
}

fn starts_with_phrase(words: &[String], phrase: &[&str]) -> bool {
    words.len() >= phrase.len() && words.iter().zip(phrase).all(|(w, p)| w == p)
}

fn split_on_connectors(tokens: Vec<String>) -> Vec<Vec<String>> {
    let words = lower(&tokens);
    let mut pieces = vec![Vec::new()];
    let mut i = 0;
    while i < tokens.len() {
        if let Some(phrase) = CONNECTORS.iter().find(|p| starts_with_phrase(&words[i..], p)) {
            pieces.push(Vec::new());
            i += phrase.len();
        } else {
            pieces.last_mut().unwrap().push(tokens[i].clone());
            i += 1;
        }
    }
    pieces.retain(|p| !p.is_empty());
    pieces
}

/// Hour number at the start of `words`: "2", "14", "14:30", "2.30", "3pm", "dua", "dua belas", "setengah tiga".
/// Returns (hour, minute, token count, am/pm marker from the token itself).
fn clock_at(words: &[String]) -> Option<(u8, u8, usize, Option<bool>)> {
    let first = words.first()?;
    if first == "setengah" {
        // "setengah tiga" = 2:30, "setengah satu" = 12:30.
        let (h, _, used, pm) = clock_at(&words[1..])?;
        return Some((if h <= 1 { h + 11 } else { h - 1 }, 30, used + 1, pm));
    }
    if let Some(n) = number_word(first) {
        if n == 2 && words.get(1).map(String::as_str) == Some("belas") {
            return Some((12, 0, 2, None));
        }
        return Some((n, 0, 1, None));
    }
    let (digits, pm) = match first.strip_suffix("pm") {
        Some(d) => (d, Some(true)),
        None => match first.strip_suffix("am") {
            Some(d) => (d, Some(false)),
            None => (first.as_str(), None),
        },
    };
    let mut parts = digits.split([':', '.']);
    let h: u8 = parts.next()?.parse().ok()?;
    let m: u8 = match parts.next() {
        Some(m) => m.parse().ok()?,
        None => 0,
    };
    (parts.next().is_none() && h < 24 && m < 60).then_some((h, m, 1, pm))
}

/// Adjusts a 12-hour time using "pagi/siang/sore/malam" (morning/noon/afternoon/night) or am/pm. Without a qualifier,
/// hours 1–6 are taken as afternoon/evening (work hours): "jam 2 meeting" = 14:00.
fn to_24h(h: u8, period: Option<&str>) -> u8 {
    match period {
        Some("pagi") | Some("am") => h % 12,
        Some("siang") => if h <= 6 { h + 12 } else { h },
        Some("sore") | Some("pm") => if h < 12 { h + 12 } else { h },
        Some("malam") => if h == 12 { 0 } else if (5..12).contains(&h) { h + 12 } else { h },
        _ if (1..=6).contains(&h) => h + 12,
        _ => h,
    }
}

/// Finds a time expression anywhere in a chunk: (hour, minute), start index, end index (exclusive).
fn find_time(words: &[String]) -> Option<((u8, u8), usize, usize)> {
    for i in 0..words.len() {
        let marked = TIME_MARKERS.contains(&words[i].as_str());
        let start = if marked { i + 1 } else { i };
        let Some((h, m, used, suffix)) = clock_at(&words[start..]) else { continue };
        // Without the "jam" marker, only accept forms that are clearly times: "14:30", "14.30", "3pm".
        if !marked && suffix.is_none() && !words[i].contains([':', '.']) {
            continue;
        }
        let mut end = start + used;
        let period = match suffix {
            Some(true) => Some("pm"),
            Some(false) => Some("am"),
            None => words.get(end).map(String::as_str).filter(|p| matches!(*p, "pagi" | "siang" | "sore" | "malam" | "am" | "pm")),
        };
        if suffix.is_none() && period.is_some() {
            end += 1;
        }
        return Some(((to_24h(h, period), m), i, end));
    }
    None
}

fn strip_phrases(tokens: &mut Vec<String>, phrases: &[&[&str]], leading: bool) {
    loop {
        let words = lower(tokens);
        let hit = phrases.iter().find(|p| {
            if leading {
                starts_with_phrase(&words, p)
            } else {
                words.len() >= p.len() && starts_with_phrase(&words[words.len() - p.len()..], p)
            }
        });
        let Some(p) = hit else { break };
        if leading {
            tokens.drain(..p.len());
        } else {
            tokens.truncate(tokens.len() - p.len());
        }
    }
}

fn parse_piece(mut tokens: Vec<String>) -> Option<Parsed> {
    let mut tomorrow = false;
    tokens.retain(|t| {
        let w = t.to_lowercase();
        if w == "besok" || w == "tomorrow" {
            tomorrow = true;
        }
        w != "besok" && w != "tomorrow"
    });

    let time = find_time(&lower(&tokens)).map(|(hm, start, end)| {
        tokens.drain(start..end);
        hm
    });

    strip_phrases(&mut tokens, LEADING_FILLERS, true);
    strip_phrases(&mut tokens, TRAILING_FILLERS, false);

    let joined = tokens.join(" ");
    let title = joined.trim_matches(|c: char| c.is_whitespace() || ",.;:-".contains(c));
    let mut chars = title.chars();
    let first = chars.next()?;
    Some(Parsed { title: first.to_uppercase().chain(chars).collect(), time, tomorrow })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(input: &str) -> Vec<String> {
        parse(input).into_iter().map(|p| p.title).collect()
    }

    #[test]
    fn splits_spoken_sentence_into_tasks() {
        let out = parse(
            "hari ini aku mau review PR auth yang 214, terus jam dua ada meeting sprint sama tim, \
             sama jangan lupa bales email klien soal invoice Oktober",
        );
        let got: Vec<(&str, Option<(u8, u8)>)> = out.iter().map(|p| (p.title.as_str(), p.time)).collect();
        assert_eq!(
            got,
            vec![
                ("Review PR auth yang 214", None),
                ("Meeting sprint sama tim", Some((14, 0))),
                ("Bales email klien soal invoice Oktober", None),
            ]
        );
    }

    #[test]
    fn keeps_and_inside_a_task() {
        assert_eq!(titles("review dan merge PR"), vec!["Review dan merge PR"]);
    }

    #[test]
    fn tomorrow_and_period_of_day() {
        let p = &parse("besok jam 9 pagi standup tim")[0];
        assert_eq!((p.title.as_str(), p.time, p.tomorrow), ("Standup tim", Some((9, 0)), true));
        assert_eq!(parse("meeting jam setengah tiga sore")[0].time, Some((14, 30)));
        assert_eq!(parse("deploy jam 12 malam")[0].time, Some((0, 0)));
        assert_eq!(parse("makan malam jam 7 malam")[0].time, Some((19, 0)));
        assert_eq!(parse("rapat jam dua belas")[0].time, Some((12, 0)));
    }

    #[test]
    fn sentences_and_decimal_times() {
        let out = parse("beli susu. telepon ibu jam 19.30");
        assert_eq!(out.len(), 2);
        assert_eq!((out[1].title.as_str(), out[1].time), ("Telepon ibu", Some((19, 30))));
        assert_eq!(parse("sync 14:30 dengan tim")[0].time, Some((14, 30)));
    }

    #[test]
    fn english() {
        let out = parse("call John at 3pm, then send invoice");
        assert_eq!(titles("call John at 3pm, then send invoice"), vec!["Call John", "Send invoice"]);
        assert_eq!(out[0].time, Some((15, 0)));
    }

    #[test]
    fn plain_numbers_are_not_times() {
        let p = &parse("baca 2 bab buku")[0];
        assert_eq!((p.title.as_str(), p.time), ("Baca 2 bab buku", None));
    }

    #[test]
    fn empty_input() {
        assert!(parse("").is_empty());
        assert!(parse("  , ;  ").is_empty());
        assert!(parse("hari ini aku mau").is_empty());
    }
}
